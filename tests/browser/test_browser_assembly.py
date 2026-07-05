import pytest

from echo_agent.agent.browser.session import BrowserSessionManager
from echo_agent.agent.tools.browser import BrowserTool


class _Cfg:
    enabled = True
    max_sessions = 3
    session_idle_timeout_sec = 300
    max_snapshot_chars = 8000
    headless = True
    nav_timeout_sec = 30
    allow_private_addresses = False


class _FakeLocator:
    def __init__(self, label):
        self.label = label
        self.clicked = False
        self.filled = None

    async def click(self, **k):
        self.clicked = True

    async def fill(self, t, **k):
        self.filled = t


class _FakePage:
    def __init__(self):
        # Two interactive elements. With the single-traversal snapshot builder,
        # @e1 → get_by_role('button', name='ok') and @e2 → name='cancel'. The
        # fake get_by_role hands back the matching locator instance so the test
        # can observe which one was actually clicked.
        self._ok = _FakeLocator("ok")
        self._cancel = _FakeLocator("cancel")
        self.url = ""

    async def goto(self, url, **k):
        self.url = url

    class _AX:
        async def snapshot(self):
            return {"role": "WebArea", "name": "p",
                    "children": [{"role": "button", "name": "ok"},
                                 {"role": "button", "name": "cancel"}]}

    @property
    def accessibility(self):
        return self._AX()

    def get_by_role(self, role, name=None):
        loc = self._ok if name == "ok" else self._cancel
        return _NthWrapper(loc)


class _NthWrapper:
    """get_by_role(...).nth(k) returns the underlying locator."""

    def __init__(self, loc):
        self._loc = loc

    def nth(self, k):
        return self._loc


@pytest.mark.asyncio
async def test_full_open_navigate_click_close(monkeypatch):
    mgr = BrowserSessionManager()

    fake_page = _FakePage()

    # fake the browser bootstrap
    class _Ctx:
        async def new_page(self): return fake_page
        async def close(self): pass
    class _Br:
        async def new_context(self): return _Ctx()
    class _PW:
        class chromium:
            @staticmethod
            async def launch(**k): return _Br()
        async def stop(self): pass
    import echo_agent.agent.browser.session as sess_mod
    monkeypatch.setattr(sess_mod, "_start_playwright", lambda: _async_val(_PW()))
    # navigate SSRF allow
    import echo_agent.agent.browser.actions as act_mod
    monkeypatch.setattr(act_mod, "check_url_ssrf", lambda url: _async_val(None))

    tool = BrowserTool(config=_Cfg(), manager=mgr)
    r_open = await tool.execute({"action": "open"})
    assert r_open.success
    sid = r_open.output.split(": ")[1]

    r_nav = await tool.execute({"action": "navigate", "session_id": sid, "url": "https://example.com"})
    assert r_nav.success
    assert "@e1" in r_nav.output  # snapshot has the button
    assert "@e2" in r_nav.output  # ...and the second interactive element

    # ref-order contract: @e1 must resolve to the FIRST query_selector_all handle
    # (ok), @e2 to the SECOND (cancel). Assert the *specific* locator is driven,
    # not merely that click did not raise — this catches a snapshot/DOM order
    # mismatch that would silently click the wrong element.
    r_click1 = await tool.execute({"action": "click", "session_id": sid, "ref": "@e1"})
    assert r_click1.success
    assert fake_page._ok.clicked is True
    assert fake_page._cancel.clicked is False

    r_click2 = await tool.execute({"action": "click", "session_id": sid, "ref": "@e2"})
    assert r_click2.success
    assert fake_page._cancel.clicked is True

    r_close = await tool.execute({"action": "close", "session_id": sid})
    assert r_close.success


def _async_val(v):
    async def _c(): return v
    return _c()


def _browser_tool_names(config):
    from pathlib import Path

    from echo_agent.agent.tools import discover_tools
    from echo_agent.bus.queue import MessageBus
    tools = discover_tools(config=config, workspace=Path("/tmp/ws_browser_gate"),
                           bus=MessageBus())
    return [t for t in tools if t.name == "browser"]


def test_assembly_includes_browser_when_enabled():
    from echo_agent.config.schema import Config
    # network must be allowed AND browser enabled for the tool to mount
    config = Config(tools={"browser": {"enabled": True}},
                    execution={"network_policy": "allow"})
    assert _browser_tool_names(config), "browser tool should mount when enabled + network allowed"


def test_assembly_skips_when_disabled():
    from echo_agent.config.schema import Config
    config = Config(tools={"browser": {"enabled": False}},
                    execution={"network_policy": "allow"})
    assert _browser_tool_names(config) == [], "browser tool must not mount when disabled"


def test_assembly_skips_when_network_deny():
    from echo_agent.config.schema import Config
    # enabled but network denied → gated off
    config = Config(tools={"browser": {"enabled": True}},
                    execution={"network_policy": "deny"})
    assert _browser_tool_names(config) == [], "browser tool must not mount under network deny"
