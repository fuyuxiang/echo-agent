"""End-to-end assembly: tool discovery gating plus a full open→act→close run."""

from pathlib import Path

import pytest

from echo_agent.agent.browser import actions as act_mod
from echo_agent.agent.browser.session import BrowserSessionManager
from echo_agent.agent.tools.browser import BrowserTool
from echo_agent.tools.base import ToolExecutionContext

from ._fakes import Cfg, FakePage, FakePlaywright, element, make_payload, patch_playwright


def _two_buttons():
    """Two buttons sharing a role; only the xpath tells them apart."""
    return make_payload([
        element("button", "ok", "/html/body/button[1]"),
        element("button", "ok", "/html/body/button[2]"),
    ])


@pytest.mark.asyncio
async def test_full_open_navigate_click_close(monkeypatch, tmp_path):
    async def _no_ssrf(url):
        return None

    page = FakePage([_two_buttons() for _ in range(8)])
    patch_playwright(monkeypatch, FakePlaywright(page))
    monkeypatch.setattr(act_mod, "check_url_ssrf", _no_ssrf)

    tool = BrowserTool(config=Cfg(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    ctx = ToolExecutionContext(session_key="conv-a")

    r_open = await tool.execute({"action": "open"}, ctx)
    assert r_open.success
    sid = r_open.metadata["session_id"]

    r_nav = await tool.execute(
        {"action": "navigate", "session_id": sid, "url": "https://example.com"}, ctx)
    assert r_nav.success
    assert "@e1" in r_nav.output and "@e2" in r_nav.output

    # Ref→element contract: both buttons have role=button name='ok', so a
    # role+name lookup would target the same node for @e1 and @e2. Assert the
    # distinct locators are driven, which is what catches that mis-targeting.
    assert (await tool.execute(
        {"action": "click", "session_id": sid, "ref": "@e1"}, ctx)).success
    assert page.locators["xpath=/html/body/button[1]"].clicked == 1
    assert page.locators["xpath=/html/body/button[2]"].clicked == 0

    assert (await tool.execute(
        {"action": "click", "session_id": sid, "ref": "@e2"}, ctx)).success
    assert page.locators["xpath=/html/body/button[2]"].clicked == 1

    assert (await tool.execute({"action": "close", "session_id": sid}, ctx)).success
    assert tool._mgr.get(sid) is None


@pytest.mark.asyncio
async def test_refs_are_renumbered_after_the_page_changes(monkeypatch, tmp_path):
    """Stale refs must not silently resolve to a different element."""
    first = make_payload([element("button", "a", "/html/body/button[1]"),
                          element("button", "b", "/html/body/button[2]")])
    second = make_payload([element("button", "b", "/html/body/button[2]")])
    page = FakePage([first, second, second])
    patch_playwright(monkeypatch, FakePlaywright(page))

    tool = BrowserTool(config=Cfg(), manager=BrowserSessionManager(),
                       workspace=str(tmp_path))
    ctx = ToolExecutionContext(session_key="conv-a")
    sid = (await tool.execute({"action": "open"}, ctx)).metadata["session_id"]

    await tool.execute({"action": "snapshot", "session_id": sid}, ctx)
    await tool.execute({"action": "snapshot", "session_id": sid}, ctx)
    res = await tool.execute({"action": "click", "session_id": sid, "ref": "@e2"}, ctx)
    assert res.success is False
    assert "@e2" in res.error and "snapshot" in res.error


def _browser_tools(config):
    from echo_agent.agent.tools import discover_tools
    from echo_agent.bus.queue import MessageBus
    tools = discover_tools(config=config, workspace=Path("/tmp/ws_browser_gate"),
                           bus=MessageBus())
    return [t for t in tools if t.name == "browser"]


def test_assembly_includes_browser_when_enabled():
    from echo_agent.config.schema import Config
    config = Config(tools={"browser": {"enabled": True}},
                    execution={"network_policy": "allow"})
    mounted = _browser_tools(config)
    assert mounted, "browser tool should mount when enabled + network allowed"
    # The workspace must be wired through, or screenshots and login state have
    # nowhere to go.
    assert mounted[0]._workspace


def test_assembly_skips_when_disabled():
    from echo_agent.config.schema import Config
    config = Config(tools={"browser": {"enabled": False}},
                    execution={"network_policy": "allow"})
    assert _browser_tools(config) == [], "browser tool must not mount when disabled"


def test_assembly_skips_when_network_deny():
    from echo_agent.config.schema import Config
    config = Config(tools={"browser": {"enabled": True}},
                    execution={"network_policy": "deny"})
    assert _browser_tools(config) == [], "browser tool must not mount under network deny"
