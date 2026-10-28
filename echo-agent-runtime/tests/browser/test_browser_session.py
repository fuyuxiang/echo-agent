"""Session lifecycle tests: launch, owner isolation, dialogs, storage state."""

import asyncio
import json
import time

import pytest

from echo_agent.agent.browser import session as sess_mod
from echo_agent.agent.browser.session import (
    BrowserLaunchError,
    BrowserSession,
    BrowserSessionManager,
)

from ._fakes import FakePage, FakePlaywright, patch_playwright


@pytest.fixture
def pw(monkeypatch):
    return patch_playwright(monkeypatch, FakePlaywright(FakePage()))


def _bare_session(**kwargs):
    return BrowserSession(context=object(), page=FakePage(), last_active=0.0, **kwargs)


# --- lifecycle --------------------------------------------------------------

@pytest.mark.asyncio
async def test_open_returns_id_and_stores_session(pw):
    m = BrowserSessionManager()
    sid = await m.open()
    assert sid.startswith("sess_")
    assert m.get(sid) is not None


@pytest.mark.asyncio
async def test_open_installs_route_gate_and_event_handlers(pw):
    m = BrowserSessionManager()
    sid = await m.open()
    ctx = m.get(sid).context
    assert ctx.routes == ["**/*"]
    # Bound on the context so popups inherit them.
    assert set(ctx.events) == {"dialog", "console", "weberror"}


@pytest.mark.asyncio
async def test_open_passes_viewport_and_user_agent(pw):
    m = BrowserSessionManager()
    await m.open(viewport_width=800, viewport_height=600, user_agent="UA/1")
    kwargs = pw.browser.context_kwargs[-1]
    assert kwargs["viewport"] == {"width": 800, "height": 600}
    assert kwargs["user_agent"] == "UA/1"


@pytest.mark.asyncio
async def test_open_omits_user_agent_when_unset(pw):
    m = BrowserSessionManager()
    await m.open()
    assert "user_agent" not in pw.browser.context_kwargs[-1]


@pytest.mark.asyncio
async def test_close_removes_session_and_closes_context(pw):
    m = BrowserSessionManager()
    sid = await m.open()
    ctx = m.get(sid).context
    assert await m.close(sid) is True
    assert m.get(sid) is None
    assert ctx.closed is True


@pytest.mark.asyncio
async def test_close_unknown_returns_false(pw):
    m = BrowserSessionManager()
    assert await m.close("nope") is False


@pytest.mark.asyncio
async def test_reap_idle_closes_stale_keeps_active(pw):
    m = BrowserSessionManager()
    stale = await m.open()
    fresh = await m.open()
    m.get(stale).last_active = time.monotonic() - 1000
    await m._reap_idle(idle_timeout_sec=300)
    assert m.get(stale) is None
    assert m.get(fresh) is not None


@pytest.mark.asyncio
async def test_close_all_stops_browser_and_playwright(pw):
    m = BrowserSessionManager()
    await m.open()
    browser, driver = m._browser, m._pw
    await m.close_all()
    assert browser.closed is True and driver.stopped is True
    assert m._browser is None and m._pw is None
    assert m.get_count() == 0


@pytest.mark.asyncio
async def test_close_all_reopens_after_stop(pw):
    m = BrowserSessionManager()
    await m.open()
    await m.close_all()
    sid = await m.open()
    assert m.get(sid) is not None
    assert m._browser is not None and m._pw is not None


# --- launch failures --------------------------------------------------------

@pytest.mark.asyncio
async def test_missing_chromium_raises_actionable_hint(monkeypatch):
    """The bare playwright error tells the user nothing; the hint names the fix."""
    patch_playwright(monkeypatch, FakePlaywright(
        launch_exc=Exception("Executable doesn't exist at /ms-playwright/chromium")))
    m = BrowserSessionManager()
    with pytest.raises(BrowserLaunchError) as exc:
        await m.open()
    assert "playwright install chromium" in str(exc.value)


@pytest.mark.asyncio
async def test_other_launch_failure_is_wrapped(monkeypatch):
    patch_playwright(monkeypatch, FakePlaywright(launch_exc=Exception("no /dev/shm")))
    m = BrowserSessionManager()
    with pytest.raises(BrowserLaunchError) as exc:
        await m.open()
    assert "浏览器启动失败" in str(exc.value)


@pytest.mark.asyncio
async def test_concurrent_open_launches_browser_once(pw):
    """Without the launch lock the loser's Chromium process leaks."""
    m = BrowserSessionManager()
    sids = await asyncio.gather(*(m.open() for _ in range(5)))
    assert len(set(sids)) == 5
    assert pw.launch_count == 1


# --- owner isolation --------------------------------------------------------

@pytest.mark.asyncio
async def test_get_denies_other_owner(pw):
    m = BrowserSessionManager()
    sid = await m.open(owner="conv-a")
    assert m.get(sid, owner="conv-a") is not None
    assert m.get(sid, owner="conv-b") is None


@pytest.mark.asyncio
async def test_close_denies_other_owner(pw):
    """A leaked session id must not let another conversation kill the session."""
    m = BrowserSessionManager()
    sid = await m.open(owner="conv-a")
    assert await m.close(sid, owner="conv-b") is False
    assert m.get(sid, owner="conv-a") is not None


@pytest.mark.asyncio
async def test_ownerless_session_stays_reachable(pw):
    m = BrowserSessionManager()
    sid = await m.open()
    assert m.get(sid, owner="anyone") is not None


@pytest.mark.asyncio
async def test_get_count_and_limit_are_per_owner(pw):
    """One busy conversation must not exhaust the pool for everyone else."""
    m = BrowserSessionManager()
    await m.open(owner="a")
    await m.open(owner="a")
    await m.open(owner="b")
    assert m.get_count() == 3
    assert m.get_count("a") == 2
    assert m._enforce_limit(2, owner="a") is False
    assert m._enforce_limit(2, owner="b") is True


@pytest.mark.asyncio
async def test_check_limits_reports_which_cap_was_hit(pw):
    """The caller needs the scope to tell the model whether waiting will help:
    an owner cap clears when it closes its own session, a total cap does not."""
    m = BrowserSessionManager()
    await m.open(owner="a")
    await m.open(owner="a")
    await m.open(owner="b")
    assert m.check_limits(max_per_owner=2, max_total=10, owner="a") == (False, "owner")
    assert m.check_limits(max_per_owner=5, max_total=3, owner="b") == (False, "total")
    assert m.check_limits(max_per_owner=5, max_total=10, owner="b") == (True, "")


@pytest.mark.asyncio
async def test_check_limits_treats_non_positive_total_as_unlimited(pw):
    m = BrowserSessionManager()
    await m.open(owner="a")
    assert m.check_limits(max_per_owner=5, max_total=0, owner="b") == (True, "")
    assert m.check_limits(max_per_owner=5, max_total=-1, owner="b") == (True, "")


@pytest.mark.asyncio
async def test_check_limits_counts_the_whole_pool_not_just_the_owner(pw):
    """Per-owner quotas alone let N conversations launch N×max chromiums."""
    m = BrowserSessionManager()
    for owner in ("a", "b", "c"):
        await m.open(owner=owner)
    ok, scope = m.check_limits(max_per_owner=3, max_total=3, owner="d")
    assert (ok, scope) == (False, "total")


# --- dialogs / console ------------------------------------------------------

class _FakeDialog:
    def __init__(self, kind="alert", message="hi"):
        self.type = kind
        self.message = message
        self.accepted = False
        self.dismissed = False

    async def accept(self):
        self.accepted = True

    async def dismiss(self):
        self.dismissed = True


@pytest.mark.asyncio
async def test_dialog_dismissed_by_default_and_recorded():
    s = _bare_session()
    dialog = _FakeDialog("confirm", "sure?")
    await sess_mod._dialog_handler(s, dialog)
    assert dialog.dismissed is True and dialog.accepted is False
    assert s.dialogs == [{"type": "confirm", "message": "sure?"}]


@pytest.mark.asyncio
async def test_dialog_accepted_under_accept_policy():
    s = _bare_session(dialog_policy="accept")
    dialog = _FakeDialog()
    await sess_mod._dialog_handler(s, dialog)
    assert dialog.accepted is True


@pytest.mark.asyncio
async def test_dialog_answer_failure_is_swallowed():
    class _Broken(_FakeDialog):
        async def dismiss(self):
            raise RuntimeError("already handled")

    s = _bare_session()
    await sess_mod._dialog_handler(s, _Broken())
    assert len(s.dialogs) == 1  # still recorded


def test_dialogs_are_capped_and_drained():
    s = _bare_session()
    for i in range(25):
        s.record_dialog({"type": "alert", "message": str(i)})
    assert len(s.dialogs) == sess_mod._MAX_RECORDED_DIALOGS
    assert s.dialogs[-1]["message"] == "24"
    assert len(s.take_dialogs()) == sess_mod._MAX_RECORDED_DIALOGS
    assert s.dialogs == []


class _FakeConsoleMessage:
    def __init__(self, kind, text):
        self.type = kind
        self.text = text


@pytest.mark.parametrize("kind,kept", [("error", True), ("warning", True),
                                       ("log", False), ("debug", False)])
def test_console_keeps_only_errors_and_warnings(kind, kept):
    s = _bare_session()
    sess_mod._console_handler(s, _FakeConsoleMessage(kind, "boom"))
    assert bool(s.console) is kept


def test_console_is_capped():
    s = _bare_session()
    for i in range(sess_mod._MAX_CONSOLE_MESSAGES + 50):
        s.record_console(str(i))
    assert len(s.console) == sess_mod._MAX_CONSOLE_MESSAGES


class _FakeError:
    name = "TypeError"
    message = "x is not a function"


class _FakeWebError:
    """Context-level weberror wraps the real error; its own str() is a repr."""

    def __init__(self):
        self.error = _FakeError()

    def __str__(self):
        return "<playwright._impl._web_error.WebError object at 0x1>"


def test_page_error_unwraps_weberror_message():
    s = _bare_session()
    sess_mod._page_error_handler(s, _FakeWebError())
    assert s.console == ["[pageerror] TypeError: x is not a function"]


def test_page_error_accepts_bare_error():
    s = _bare_session()
    sess_mod._page_error_handler(s, _FakeError())
    assert s.console == ["[pageerror] TypeError: x is not a function"]


def test_page_error_falls_back_to_repr():
    class _Opaque:
        def __str__(self):
            return ""

    s = _bare_session()
    sess_mod._page_error_handler(s, _Opaque())
    assert s.console and "_Opaque" in s.console[0]


# --- SSRF route gate --------------------------------------------------------
# The gate itself is covered in test_browser_ssrf_intercept.py; here we only
# assert it is actually installed on the context (see
# test_open_installs_route_gate_and_event_handlers above).


# --- storage state ----------------------------------------------------------

@pytest.mark.asyncio
async def test_storage_state_loaded_when_file_exists(pw, tmp_path):
    state = tmp_path / "owner.json"
    state.write_text(json.dumps({"cookies": [], "origins": []}))
    m = BrowserSessionManager()
    await m.open(storage_state_path=str(state))
    assert pw.browser.context_kwargs[-1]["storage_state"] == str(state)


@pytest.mark.asyncio
async def test_storage_state_absent_file_is_not_passed(pw, tmp_path):
    m = BrowserSessionManager()
    await m.open(storage_state_path=str(tmp_path / "missing.json"))
    assert "storage_state" not in pw.browser.context_kwargs[-1]


@pytest.mark.asyncio
async def test_corrupt_storage_state_falls_back_to_blank_context(pw, tmp_path):
    """A bad state file must not make the browser unusable."""
    state = tmp_path / "owner.json"
    state.write_text("not json")
    m = BrowserSessionManager()
    pw.browser  # browser is created lazily on first launch
    sid = await m.open(storage_state_path=str(state))
    assert m.get(sid) is not None
    assert "storage_state" not in pw.browser.context_kwargs[-1]


@pytest.mark.asyncio
async def test_save_storage_state_writes_file(pw, tmp_path):
    state = tmp_path / "nested" / "owner.json"
    m = BrowserSessionManager()
    sid = await m.open(owner="a", storage_state_path=str(state))
    assert await m.save_storage_state(sid, owner="a") == str(state)
    assert state.is_file()


@pytest.mark.asyncio
async def test_save_storage_state_without_path_is_noop(pw):
    m = BrowserSessionManager()
    sid = await m.open()
    assert await m.save_storage_state(sid) == ""


@pytest.mark.asyncio
async def test_save_storage_state_swallows_failure(pw, tmp_path):
    state = tmp_path / "owner.json"
    m = BrowserSessionManager()
    sid = await m.open(storage_state_path=str(state))
    m.get(sid).context.storage_exc = RuntimeError("context gone")
    assert await m.save_storage_state(sid) == ""


@pytest.mark.asyncio
async def test_close_flushes_login_state(pw, tmp_path):
    """Closing must persist the login, or the next task starts logged out."""
    state = tmp_path / "owner.json"
    m = BrowserSessionManager()
    sid = await m.open(storage_state_path=str(state))
    assert await m.close(sid) is True
    assert state.is_file()


@pytest.mark.asyncio
async def test_close_still_closes_when_flush_fails(pw, tmp_path):
    state = tmp_path / "owner.json"
    m = BrowserSessionManager()
    sid = await m.open(storage_state_path=str(state))
    ctx = m.get(sid).context
    ctx.storage_exc = RuntimeError("context gone")
    assert await m.close(sid) is True
    assert ctx.closed is True
