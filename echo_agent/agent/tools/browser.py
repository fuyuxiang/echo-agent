from __future__ import annotations

from typing import Any

from echo_agent.agent.browser import actions as _actions
from echo_agent.agent.browser.session import BrowserSessionManager
from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult

_READONLY_ACTIONS = {"snapshot", "screenshot"}
_KNOWN_ACTIONS = {"open", "navigate", "snapshot", "click", "type", "screenshot", "close"}


def _playwright_available() -> bool:
    try:
        import playwright  # noqa: F401
        return True
    except Exception:
        return False


class BrowserTool(Tool):
    name = "browser"
    description = (
        "Drive a real browser for multi-step web interaction. Actions: "
        "open (start a session→session_id), navigate (session_id+url), "
        "snapshot (session_id→ref-annotated page text), click (session_id+ref), "
        "type (session_id+ref+text), screenshot (session_id), close (session_id). "
        "Refs (@e1/@e2) come from the latest snapshot and are re-numbered each snapshot."
    )
    parameters = {
        "type": "object",
        "properties": {
            "action": {"type": "string",
                       "enum": ["open", "navigate", "snapshot", "click", "type", "screenshot", "close"],
                       "description": "Action to perform."},
            "session_id": {"type": "string", "description": "Session id from open."},
            "url": {"type": "string", "description": "URL for navigate."},
            "ref": {"type": "string", "description": "Element ref (@e1) for click/type."},
            "text": {"type": "string", "description": "Text to type."},
        },
        "required": ["action"],
    }
    timeout_seconds = 60
    risk_level = "exec"

    def __init__(self, *, config: Any, manager: BrowserSessionManager | None = None) -> None:
        self._cfg = config
        self._mgr = manager or BrowserSessionManager()

    def readiness_detail(self) -> tuple[bool, str]:
        if not _playwright_available():
            return False, "playwright not installed"
        return True, "ok"

    def is_ready(self) -> bool:
        return self.readiness_detail()[0]

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only" if params.get("action") in _READONLY_ACTIONS else "side_effect"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params.get("action", "")
        if action not in _KNOWN_ACTIONS:
            return ToolResult(success=False, error=f"Unknown action: {action}")
        await self._mgr._reap_idle(self._cfg.session_idle_timeout_sec)

        if action == "open":
            if not self._mgr._enforce_limit(self._cfg.max_sessions):
                return ToolResult(success=False, error="会话已达上限，请先 close 一个会话")
            sid = await self._mgr.open(
                headless=self._cfg.headless,
                allow_private=self._cfg.allow_private_addresses,
            )
            return ToolResult(output=f"browser session opened: {sid}")

        if action == "close":
            ok = await self._mgr.close(params.get("session_id", ""))
            return ToolResult(output="closed" if ok else "会话不存在或已回收")

        # remaining actions need a live session
        session = self._mgr.get(params.get("session_id", ""))
        if session is None:
            return ToolResult(success=False, error="会话不存在或已回收，请先 open")

        if action == "navigate":
            err = await _actions.navigate(
                session, params.get("url", ""),
                timeout_sec=self._cfg.nav_timeout_sec,
                allow_private=self._cfg.allow_private_addresses,
            )
            if err:
                return ToolResult(success=False, error=err)
            text = await _actions.refresh_snapshot(session, max_chars=self._cfg.max_snapshot_chars)
            return ToolResult(output=text)

        if action == "snapshot":
            text = await _actions.refresh_snapshot(session, max_chars=self._cfg.max_snapshot_chars)
            return ToolResult(output=text)

        if action == "click":
            err = await _actions.click(session, params.get("ref", ""))
            if err:
                return ToolResult(success=False, error=err)
            text = await _actions.refresh_snapshot(session, max_chars=self._cfg.max_snapshot_chars)
            return ToolResult(output=text)

        if action == "type":
            err = await _actions.type_text(session, params.get("ref", ""), params.get("text", ""))
            if err:
                return ToolResult(success=False, error=err)
            text = await _actions.refresh_snapshot(session, max_chars=self._cfg.max_snapshot_chars)
            return ToolResult(output=text)

        if action == "screenshot":
            data = await _actions.screenshot(session)
            return ToolResult(output=f"captured {len(data)} bytes", metadata={"image_bytes": len(data)})

        return ToolResult(success=False, error=f"Unknown action: {action}")
