import pytest

from echo_agent.agent.tools.browser import BrowserTool


class _Cfg:
    enabled = True
    max_sessions = 3
    session_idle_timeout_sec = 300
    max_snapshot_chars = 8000
    headless = True
    nav_timeout_sec = 30
    allow_private_addresses = False


@pytest.mark.asyncio
async def test_unknown_action_errors():
    tool = BrowserTool(config=_Cfg())
    res = await tool.execute({"action": "fly"})
    assert res.success is False
    assert "action" in res.error.lower()


@pytest.mark.asyncio
async def test_navigate_missing_session_errors():
    tool = BrowserTool(config=_Cfg())
    res = await tool.execute({"action": "navigate", "session_id": "nope", "url": "https://x.com"})
    assert res.success is False
    assert "会话" in res.error


@pytest.mark.asyncio
async def test_click_missing_session_errors():
    tool = BrowserTool(config=_Cfg())
    res = await tool.execute({"action": "click", "session_id": "nope", "ref": "@e1"})
    assert res.success is False
    assert "会话" in res.error


def test_execution_mode_readonly_for_snapshot():
    tool = BrowserTool(config=_Cfg())
    assert tool.execution_mode({"action": "snapshot"}) == "read_only"
    assert tool.execution_mode({"action": "screenshot"}) == "read_only"
    assert tool.execution_mode({"action": "click"}) == "side_effect"


def test_is_ready_false_without_playwright(monkeypatch):
    import echo_agent.agent.tools.browser as bmod
    monkeypatch.setattr(bmod, "_playwright_available", lambda: False)
    tool = BrowserTool(config=_Cfg())
    ready, reason = tool.readiness_detail()
    assert ready is False


def test_risk_level_is_exec():
    assert BrowserTool.risk_level == "exec"
