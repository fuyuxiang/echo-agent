"""Tool-layer tests: action routing, isolation, result shape, artifacts."""

import pytest

from echo_agent.agent.browser.session import BrowserSessionManager
from echo_agent.agent.tools.browser import _KNOWN_ACTIONS, BrowserTool
from echo_agent.tools.base import ToolExecutionContext

from ._fakes import Cfg, FakePage, FakePlaywright, element, make_payload, patch_playwright


def _payloads(n=40):
    """Enough snapshot payloads for the post-action refreshes each test triggers."""
    return [make_payload([element("button", "ok", "/html/body/button[1]")])
            for _ in range(n)]


@pytest.fixture
def page():
    return FakePage(_payloads())


@pytest.fixture
def tool(monkeypatch, page, tmp_path):
    patch_playwright(monkeypatch, FakePlaywright(page))
    return BrowserTool(config=Cfg(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))


def _ctx(session_key="conv-a"):
    return ToolExecutionContext(session_key=session_key)


async def _open(tool, ctx=None):
    res = await tool.execute({"action": "open"}, ctx or _ctx())
    assert res.success is True, res.error
    return res.metadata["session_id"]


# --- contract ---------------------------------------------------------------

def test_risk_level_and_timeout():
    assert BrowserTool.risk_level == "exec"
    assert BrowserTool.timeout_seconds >= 60


def test_declared_enum_matches_dispatch_table():
    """A schema/dispatch mismatch makes the model call actions that can't run."""
    enum = set(BrowserTool.parameters["properties"]["action"]["enum"])
    assert enum == _KNOWN_ACTIONS


@pytest.mark.parametrize("action,mode", [
    ("snapshot", "read_only"), ("screenshot", "read_only"),
    ("get_images", "read_only"), ("console", "read_only"), ("wait", "read_only"),
    ("click", "side_effect"), ("type", "side_effect"), ("evaluate", "side_effect"),
    ("navigate", "side_effect"), ("upload", "side_effect"),
])
def test_execution_mode(action, mode):
    assert BrowserTool(config=Cfg()).execution_mode({"action": action}) == mode


def test_is_ready_false_without_playwright(monkeypatch):
    import echo_agent.agent.tools.browser as bmod
    monkeypatch.setattr(bmod, "_playwright_available", lambda: False)
    ready, reason = BrowserTool(config=Cfg()).readiness_detail()
    assert ready is False and "playwright" in reason


# --- validation -------------------------------------------------------------

@pytest.mark.asyncio
async def test_unknown_action_is_validation_error():
    res = await BrowserTool(config=Cfg()).execute({"action": "fly"})
    assert res.success is False and res.error_kind == "validation"


@pytest.mark.asyncio
async def test_missing_session_id_is_validation_not_business(tool):
    """A forgotten parameter must not read as an expired session."""
    res = await tool.execute({"action": "navigate", "url": "https://x.com"}, _ctx())
    assert res.success is False
    assert res.error_kind == "validation" and "session_id" in res.error


@pytest.mark.asyncio
async def test_unknown_session_id_is_business_error(tool):
    res = await tool.execute({"action": "click", "session_id": "nope", "ref": "@e1"},
                             _ctx())
    assert res.success is False and res.error_kind == "business"
    assert "会话" in res.error


@pytest.mark.asyncio
async def test_navigate_requires_url(tool):
    sid = await _open(tool)
    res = await tool.execute({"action": "navigate", "session_id": sid}, _ctx())
    assert res.success is False and res.error_kind == "validation"


@pytest.mark.asyncio
async def test_type_requires_ref(tool):
    sid = await _open(tool)
    res = await tool.execute({"action": "type", "session_id": sid, "text": "x"}, _ctx())
    assert res.success is False and res.error_kind == "validation"


# --- session lifecycle ------------------------------------------------------

@pytest.mark.asyncio
async def test_open_returns_session_id_in_metadata(tool):
    sid = await _open(tool)
    assert sid.startswith("sess_")


@pytest.mark.asyncio
async def test_open_respects_max_sessions_per_owner(tool):
    for _ in range(Cfg.max_sessions):
        await _open(tool)
    res = await tool.execute({"action": "open"}, _ctx())
    assert res.success is False and res.error_kind == "business"
    # Another conversation still gets its own quota.
    assert (await tool.execute({"action": "open"}, _ctx("conv-b"))).success is True


@pytest.mark.asyncio
async def test_open_respects_the_global_session_cap(monkeypatch, page, tmp_path):
    """Per-owner quotas alone let N conversations launch N×max_sessions
    chromiums; the total cap is what protects the host's memory."""
    class C(Cfg):
        max_sessions = 2
        max_total_sessions = 3

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=C(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    assert (await tool.execute({"action": "open"}, _ctx("conv-a"))).success is True
    assert (await tool.execute({"action": "open"}, _ctx("conv-a"))).success is True
    assert (await tool.execute({"action": "open"}, _ctx("conv-b"))).success is True
    res = await tool.execute({"action": "open"}, _ctx("conv-b"))
    assert res.success is False and res.error_kind == "business"
    assert "总数" in res.error


@pytest.mark.asyncio
async def test_global_cap_can_be_disabled(monkeypatch, page, tmp_path):
    class C(Cfg):
        max_sessions = 1
        max_total_sessions = 0

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=C(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    for owner in ("a", "b", "c", "d"):
        assert (await tool.execute({"action": "open"}, _ctx(owner))).success is True


@pytest.mark.asyncio
async def test_launch_error_is_dependency_kind(monkeypatch, tmp_path):
    patch_playwright(monkeypatch, FakePlaywright(
        launch_exc=Exception("Executable doesn't exist")))
    tool = BrowserTool(config=Cfg(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    res = await tool.execute({"action": "open"}, _ctx())
    assert res.success is False and res.error_kind == "dependency"
    assert "playwright install chromium" in res.error


@pytest.mark.asyncio
async def test_close_reports_unknown_session_without_failing(tool):
    res = await tool.execute({"action": "close", "session_id": "gone"}, _ctx())
    assert res.success is True and "不存在" in res.output


@pytest.mark.asyncio
async def test_close_removes_the_session(tool):
    sid = await _open(tool)
    assert (await tool.execute({"action": "close", "session_id": sid}, _ctx())).success
    res = await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    assert res.success is False


# --- cross-owner isolation --------------------------------------------------

@pytest.mark.asyncio
async def test_other_owner_cannot_drive_session(tool):
    """A session id leaked into another conversation must resolve to nothing."""
    sid = await _open(tool, _ctx("conv-a"))
    res = await tool.execute({"action": "click", "session_id": sid, "ref": "@e1"},
                             _ctx("conv-b"))
    assert res.success is False and res.error_kind == "business"


@pytest.mark.asyncio
async def test_other_owner_cannot_close_session(tool):
    sid = await _open(tool, _ctx("conv-a"))
    res = await tool.execute({"action": "close", "session_id": sid}, _ctx("conv-b"))
    assert "不存在" in res.output
    assert (await tool.execute({"action": "snapshot", "session_id": sid},
                               _ctx("conv-a"))).success is True


@pytest.mark.asyncio
async def test_owner_falls_back_through_identity_fields(tool):
    ctx = ToolExecutionContext(user_id="u1")
    sid = await _open(tool, ctx)
    assert tool._mgr.get(sid, "u1") is not None
    assert tool._mgr.get(sid, "u2") is None


# --- happy-path dispatch ----------------------------------------------------

@pytest.mark.asyncio
async def test_snapshot_returns_refs(tool):
    sid = await _open(tool)
    res = await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    assert res.success is True and "@e1" in res.output


@pytest.mark.asyncio
async def test_navigate_then_snapshot_is_returned(tool, page):
    sid = await _open(tool)
    res = await tool.execute(
        {"action": "navigate", "session_id": sid, "url": "https://example.com"}, _ctx())
    assert res.success is True and "@e1" in res.output
    assert page.goto_url == "https://example.com"


@pytest.mark.asyncio
async def test_click_uses_current_ref_map(tool, page):
    sid = await _open(tool)
    await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    res = await tool.execute({"action": "click", "session_id": sid, "ref": "@e1"}, _ctx())
    assert res.success is True
    assert page.locators["xpath=/html/body/button[1]"].clicked == 1


@pytest.mark.asyncio
async def test_type_with_press_enter(tool, page):
    sid = await _open(tool)
    await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    res = await tool.execute({"action": "type", "session_id": sid, "ref": "@e1",
                              "text": "hi", "press_enter": True}, _ctx())
    assert res.success is True
    loc = page.locators["xpath=/html/body/button[1]"]
    assert loc.filled == "hi" and loc.pressed == ["Enter"]


@pytest.mark.asyncio
async def test_press_scroll_history_actions(tool, page):
    sid = await _open(tool)
    for params in (
        {"action": "press", "key": "Tab"},
        {"action": "scroll", "direction": "down", "amount": 300},
        {"action": "back"},
        {"action": "forward"},
        {"action": "reload"},
        {"action": "wait", "state": "load"},
    ):
        res = await tool.execute({**params, "session_id": sid}, _ctx())
        assert res.success is True, (params, res.error)
    assert page.keyboard.pressed == ["Tab"]
    assert page.mouse.wheels == [(0, 300)]
    assert (page.back, page.forward, page.reloaded) == (1, 1, 1)
    assert page.wait_states == ["load"]


@pytest.mark.asyncio
async def test_select_accepts_text_as_single_value(tool, page):
    sid = await _open(tool)
    await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    res = await tool.execute({"action": "select", "session_id": sid, "ref": "@e1",
                              "text": "Beta"}, _ctx())
    assert res.success is True
    assert page.locators["xpath=/html/body/button[1]"].selected.get("value") == ["Beta"]


@pytest.mark.asyncio
async def test_upload_passes_paths(tool, page, tmp_path):
    sid = await _open(tool)
    await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    res = await tool.execute({"action": "upload", "session_id": sid, "ref": "@e1",
                              "paths": [str(tmp_path / "a.txt")]}, _ctx())
    assert res.success is True
    assert page.locators["xpath=/html/body/button[1]"].files == [str(tmp_path / "a.txt")]


@pytest.mark.asyncio
async def test_hover(tool, page):
    sid = await _open(tool)
    await tool.execute({"action": "snapshot", "session_id": sid}, _ctx())
    res = await tool.execute({"action": "hover", "session_id": sid, "ref": "@e1"}, _ctx())
    assert res.success is True
    assert page.locators["xpath=/html/body/button[1]"].hovered is True


# --- failure shape ----------------------------------------------------------

@pytest.mark.asyncio
async def test_stale_ref_error_carries_a_fresh_snapshot(tool):
    """Without the page state the model just retries the same dead ref."""
    sid = await _open(tool)
    res = await tool.execute({"action": "click", "session_id": sid, "ref": "@e99"},
                             _ctx())
    assert res.success is False and res.error_kind == "business"
    assert "--- 当前页面 ---" in res.error and "@e1" in res.error


@pytest.mark.asyncio
async def test_business_errors_do_not_trip_the_circuit_breaker(tool):
    """Only timeout/dependency/internal should count against the breaker."""
    sid = await _open(tool)
    res = await tool.execute({"action": "scroll", "session_id": sid,
                              "direction": "sideways"}, _ctx())
    assert res.success is False and res.error_kind == "business"


@pytest.mark.asyncio
async def test_navigate_failure_is_dependency(monkeypatch, tool, page):
    sid = await _open(tool)
    page.goto_exc = Exception("net::ERR_NAME_NOT_RESOLVED")
    res = await tool.execute(
        {"action": "navigate", "session_id": sid, "url": "https://nope.example"}, _ctx())
    assert res.success is False and res.error_kind == "dependency"


# --- evaluate / console -----------------------------------------------------

@pytest.mark.asyncio
async def test_evaluate_returns_value(tool, page):
    page.eval_results["document.title"] = "Example"
    sid = await _open(tool)
    res = await tool.execute({"action": "evaluate", "session_id": sid,
                              "expression": "document.title"}, _ctx())
    assert res.success is True and res.output == "Example"


@pytest.mark.asyncio
async def test_evaluate_refusal_is_business_error(tool):
    sid = await _open(tool)
    res = await tool.execute({"action": "evaluate", "session_id": sid,
                              "expression": "document.cookie"}, _ctx())
    assert res.success is False and res.error_kind == "business"


@pytest.mark.asyncio
async def test_evaluate_allowed_when_config_opts_in(monkeypatch, page, tmp_path):
    class _Loose(Cfg):
        allow_unsafe_evaluate = True

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_Loose(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    page.eval_default = "a=b"
    sid = await _open(tool)
    res = await tool.execute({"action": "evaluate", "session_id": sid,
                              "expression": "document.cookie"}, _ctx())
    assert res.success is True and res.output == "a=b"


@pytest.mark.asyncio
async def test_evaluate_can_be_switched_off_entirely(monkeypatch, page, tmp_path):
    """The approval gate is per-tool, so a deployment that does not want
    arbitrary in-page JS at all needs a switch inside the tool."""
    class _NoEval(Cfg):
        allow_evaluate = False

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_NoEval(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    page.eval_results["document.title"] = "Example"
    sid = await _open(tool)
    res = await tool.execute({"action": "evaluate", "session_id": sid,
                              "expression": "document.title"}, _ctx())
    assert res.success is False and res.error_kind == "business"
    assert "allow_evaluate" in res.error
    assert page.evaluated == []
    # Everything else on the tool still works.
    assert (await tool.execute({"action": "snapshot", "session_id": sid},
                               _ctx())).success is True


@pytest.mark.asyncio
async def test_evaluate_undefined_is_labelled(tool):
    sid = await _open(tool)
    res = await tool.execute({"action": "evaluate", "session_id": sid,
                              "expression": "void 0"}, _ctx())
    assert res.success is True and "undefined" in res.output


@pytest.mark.asyncio
async def test_console_drains_recorded_lines(tool):
    sid = await _open(tool)
    tool._mgr.get(sid).record_console("[error] boom")
    res = await tool.execute({"action": "console", "session_id": sid}, _ctx())
    assert "boom" in res.output and res.metadata["console_count"] == 1
    # Draining prevents the same error being re-reported forever.
    assert "无控制台" in (
        await tool.execute({"action": "console", "session_id": sid}, _ctx())).output


# --- artifacts --------------------------------------------------------------

@pytest.mark.asyncio
async def test_screenshot_writes_file_and_exposes_path(tool, tmp_path):
    from pathlib import Path

    sid = await _open(tool)
    res = await tool.execute({"action": "screenshot", "session_id": sid,
                              "full_page": True}, _ctx())
    assert res.success is True
    path = Path(res.metadata["screenshot_path"])
    assert path.is_file() and path.read_bytes() == b"PNGDATA"
    assert res.metadata["image_bytes"] == len(b"PNGDATA")
    # The model needs to be told the artifact is consumable by vision_analyze.
    assert "vision_analyze" in res.output


@pytest.mark.asyncio
async def test_screenshot_failure_is_dependency(tool, page):
    sid = await _open(tool)
    page.screenshot_exc = RuntimeError("no renderer")
    res = await tool.execute({"action": "screenshot", "session_id": sid}, _ctx())
    assert res.success is False and res.error_kind == "dependency"


@pytest.mark.asyncio
async def test_get_images_lists_rows(tool, page):
    page.eval_results["images"] = [
        {"url": "https://x/a.png", "alt": "cat", "w": 80, "h": 60}]
    sid = await _open(tool)
    res = await tool.execute({"action": "get_images", "session_id": sid}, _ctx())
    assert "https://x/a.png" in res.output and "80x60" in res.output
    assert res.metadata["image_count"] == 1


@pytest.mark.asyncio
async def test_get_images_empty_page(tool):
    sid = await _open(tool)
    res = await tool.execute({"action": "get_images", "session_id": sid}, _ctx())
    assert res.success is True and "无可用图片" in res.output


# --- timeout_sec ------------------------------------------------------------

@pytest.mark.asyncio
async def test_navigate_honours_timeout_sec(tool, page):
    """The parameter was advertised in the schema but navigation ignored it, so a
    slow page could not be given more time nor a doomed load cut short."""
    sid = await _open(tool)
    await tool.execute({"action": "navigate", "session_id": sid,
                        "url": "https://example.com/", "timeout_sec": 5}, _ctx())
    assert page.nav_kwargs.get("timeout") == 5000


@pytest.mark.asyncio
async def test_navigate_falls_back_to_configured_timeout(tool, page):
    sid = await _open(tool)
    await tool.execute({"action": "navigate", "session_id": sid,
                        "url": "https://example.com/"}, _ctx())
    assert page.nav_kwargs.get("timeout") == Cfg.nav_timeout_sec * 1000


@pytest.mark.parametrize("action", ["back", "forward", "reload"])
@pytest.mark.asyncio
async def test_history_actions_honour_timeout_sec(tool, page, action):
    sid = await _open(tool)
    await tool.execute({"action": action, "session_id": sid, "timeout_sec": 7}, _ctx())
    assert page.nav_kwargs.get("timeout") == 7000


@pytest.mark.asyncio
async def test_timeout_sec_is_capped(tool, page):
    """An unbounded value would just hang until the outer tool timeout killed the
    call, losing both the snapshot and any usable error."""
    sid = await _open(tool)
    await tool.execute({"action": "navigate", "session_id": sid,
                        "url": "https://example.com/", "timeout_sec": 3600}, _ctx())
    assert page.nav_kwargs.get("timeout") == BrowserTool._MAX_ACTION_TIMEOUT_SEC * 1000


@pytest.mark.parametrize("bad", [0, -5, "", "abc", None])
@pytest.mark.asyncio
async def test_unusable_timeout_sec_falls_back_to_default(tool, page, bad):
    sid = await _open(tool)
    await tool.execute({"action": "navigate", "session_id": sid,
                        "url": "https://example.com/", "timeout_sec": bad}, _ctx())
    assert page.nav_kwargs.get("timeout") == Cfg.nav_timeout_sec * 1000


# --- login persistence ------------------------------------------------------

@pytest.mark.asyncio
async def test_state_path_disabled_by_default(tool):
    assert tool._state_path("conv-a") == ""


@pytest.mark.asyncio
async def test_state_path_is_per_owner_and_sanitized(monkeypatch, page, tmp_path):
    class _Persist(Cfg):
        persist_login_state = True

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_Persist(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    p1 = tool._state_path("chat/../../etc:1")
    p2 = tool._state_path("other")
    assert p1 != p2
    assert ".." not in p1 and "/etc" not in p1
    assert p1.startswith(str(tmp_path))


@pytest.mark.asyncio
async def test_state_path_distinguishes_owners_that_sanitize_alike(monkeypatch, page,
                                                                   tmp_path):
    """Sanitizing to alphanumerics alone maps 'chat/1' and 'chat:1' onto the same
    file, which would hand one conversation another's session cookies."""
    class _Persist(Cfg):
        persist_login_state = True

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_Persist(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    paths = {tool._state_path(o) for o in
             ("chat/1", "chat:1", "chat_1", "chat-1", "c/hat1", "chat1")}
    assert len(paths) == 6


@pytest.mark.asyncio
async def test_state_path_separates_long_owners_sharing_a_prefix(monkeypatch, page,
                                                                tmp_path):
    """The readable hint is truncated; the digest is what keeps them apart."""
    class _Persist(Cfg):
        persist_login_state = True

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_Persist(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    a = tool._state_path("conversation" + "a" * 40)
    b = tool._state_path("conversation" + "b" * 40)
    assert a != b


@pytest.mark.asyncio
async def test_state_path_separates_namespaces(monkeypatch, page, tmp_path):
    """Two agents sharing one conversation key must not share a login state."""
    class _Persist(Cfg):
        persist_login_state = True

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_Persist(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    assert tool._state_path("conv-a", "agent-1") != tool._state_path("conv-a", "agent-2")
    assert tool._state_path("conv-a", "") != tool._state_path("conv-a", "agent-1")


@pytest.mark.asyncio
async def test_close_persists_login_state_when_enabled(monkeypatch, page, tmp_path):
    from pathlib import Path

    class _Persist(Cfg):
        persist_login_state = True

    patch_playwright(monkeypatch, FakePlaywright(page))
    tool = BrowserTool(config=_Persist(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    sid = await _open(tool)
    await tool.execute({"action": "close", "session_id": sid}, _ctx())
    assert Path(tool._state_path("conv-a")).is_file()
