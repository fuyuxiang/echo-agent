"""Browser automation tool — drives a real Chromium via Playwright."""

from __future__ import annotations

import hashlib
import time
import uuid
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.browser import actions as _actions
from echo_agent.agent.browser.session import BrowserLaunchError, BrowserSessionManager
from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult

_READONLY_ACTIONS = {"snapshot", "screenshot", "get_images", "console", "wait"}
_KNOWN_ACTIONS = {
    "open", "navigate", "snapshot", "click", "type", "press", "scroll",
    "back", "forward", "reload", "hover", "select", "upload", "wait",
    "evaluate", "console", "screenshot", "get_images", "close",
}
# Actions whose outcome is only meaningful against a fresh view of the page.
_SNAPSHOT_AFTER = {"navigate", "click", "type", "press", "scroll", "back",
                   "forward", "reload", "select", "upload", "wait", "hover"}


def _playwright_available() -> bool:
    try:
        import playwright  # noqa: F401
        return True
    except Exception:
        return False


class BrowserTool(Tool):
    name = "browser"
    description = (
        "Drive a real browser for multi-step web interaction.\n"
        "Session: open (→session_id) / close (session_id).\n"
        "Navigate: navigate (url), back, forward, reload.\n"
        "Interact: click (ref), type (ref+text, set press_enter to submit), "
        "press (key, optional ref), scroll (direction up/down/left/right/top/bottom), "
        "hover (ref), select (ref+values), upload (ref+paths).\n"
        "Inspect: snapshot, screenshot (saves a PNG; pass its path to vision_analyze), "
        "get_images, console (JS errors), evaluate (expression), wait (text or state).\n"
        "Every action except screenshot/get_images/console/evaluate returns a fresh "
        "snapshot. Refs (@e1/@e2) come from the latest snapshot and are renumbered "
        "each time — always act on the newest ones."
    )
    parameters = {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["open", "navigate", "snapshot", "click", "type", "press",
                         "scroll", "back", "forward", "reload", "hover", "select",
                         "upload", "wait", "evaluate", "console", "screenshot",
                         "get_images", "close"],
                "description": "Action to perform.",
            },
            "session_id": {"type": "string", "description": "Session id from open."},
            "url": {"type": "string", "description": "URL for navigate."},
            "ref": {"type": "string",
                    "description": "Element ref (@e1) from the latest snapshot."},
            "text": {"type": "string",
                     "description": "Text to type, or text to wait for with action=wait."},
            "press_enter": {"type": "boolean",
                            "description": "With type: press Enter afterwards to submit."},
            "key": {"type": "string",
                    "description": "Key for press, e.g. Enter / Tab / Escape / ArrowDown / Control+a."},
            "direction": {"type": "string",
                          "enum": ["up", "down", "left", "right", "top", "bottom"],
                          "description": "Scroll direction."},
            "amount": {"type": "integer",
                       "description": "Scroll distance in pixels (default one viewport step)."},
            "values": {"type": "array", "items": {"type": "string"},
                       "description": "Option labels or values for select."},
            "paths": {"type": "array", "items": {"type": "string"},
                      "description": "File paths for upload."},
            "expression": {"type": "string",
                           "description": "JavaScript expression for evaluate."},
            "state": {"type": "string",
                      "enum": ["load", "domcontentloaded", "networkidle"],
                      "description": "Load state to wait for with action=wait."},
            "full_page": {"type": "boolean",
                          "description": "With screenshot: capture the whole scrollable page."},
            "timeout_sec": {"type": "integer",
                            "description": ("Override timeout in seconds for wait and "
                                            "navigate/back/forward/reload (capped at 120).")},
        },
        "required": ["action"],
    }
    timeout_seconds = 90
    risk_level = "exec"

    def __init__(self, *, config: Any, manager: BrowserSessionManager | None = None,
                 workspace: str = "") -> None:
        self._cfg = config
        self._mgr = manager or BrowserSessionManager()
        self._workspace = workspace

    def readiness_detail(self) -> tuple[bool, str]:
        if not _playwright_available():
            return False, "playwright not installed (pip install 'echo-agent[browser]')"
        return True, "ok"

    def is_ready(self) -> bool:
        return self.readiness_detail()[0]

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only" if params.get("action") in _READONLY_ACTIONS else "side_effect"

    def _owner(self, ctx: ToolExecutionContext | None) -> str:
        """Isolation key for a session. Falls back through the identity fields
        the loop actually populates so sessions are never globally shared."""
        if ctx is None:
            return ""
        return ctx.session_key or ctx.user_id or ctx.agent_id or ""

    def _state_namespace(self, ctx: ToolExecutionContext | None) -> str:
        """Agent scope folded into the persisted-login-state key.

        The workspace already separates deployments, but a single workspace can
        run several agents against the same session_key; without this, they would
        share one cookie jar.
        """
        if ctx is None:
            return ""
        return ctx.agent_id or ""

    def _cfg_get(self, name: str, default: Any) -> Any:
        return getattr(self._cfg, name, default)

    def _state_path(self, owner: str, namespace: str = "") -> str:
        """Path holding *owner*'s persisted cookies/localStorage, or "".

        The filename is a hash of the FULL owner key, not a sanitised copy of it.
        Replacing each non-alphanumeric character collapsed distinct owners onto
        one file — ``channel:a/b``, ``channel:a:b`` and ``channel:a?b`` all became
        ``channel_a_b.json`` — so with persist_login_state on, one user's session
        could load another's cookies. A short readable prefix is kept for
        debugging, but identity comes from the digest alone.
        """
        if not self._cfg_get("persist_login_state", False) or not self._workspace:
            return ""
        # NUL cannot appear in either component, so it is an unambiguous
        # separator: without one, ("a", "bc") and ("ab", "c") would hash alike.
        key = f"{namespace}\0{owner or 'default'}"
        digest = hashlib.sha256(key.encode("utf-8")).hexdigest()[:32]
        hint = "".join(c for c in (owner or "default") if c.isalnum())[:16] or "owner"
        return str(Path(self._workspace) / "data" / "browser_state"
                   / f"{hint}_{digest}.json")

    def _screenshot_dir(self) -> Path:
        base = Path(self._workspace) if self._workspace else Path.cwd()
        return base / "data" / "browser_screenshots"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params.get("action", "")
        if action not in _KNOWN_ACTIONS:
            return ToolResult(success=False, error=f"Unknown action: {action}",
                              error_kind="validation")
        owner = self._owner(ctx)
        await self._mgr._reap_idle(self._cfg_get("session_idle_timeout_sec", 300))

        if action == "open":
            return await self._do_open(owner, self._state_namespace(ctx))
        session_id = params.get("session_id", "")
        if not session_id:
            return ToolResult(
                success=False,
                error=f"{action} 需要 session_id（先用 action=open 获取）",
                error_kind="validation")
        if action == "close":
            ok = await self._mgr.close(session_id, owner)
            return ToolResult(output="closed" if ok else "会话不存在或已回收")

        session = self._mgr.get(session_id, owner)
        if session is None:
            return ToolResult(success=False,
                              error="会话不存在或已回收，请先 open",
                              error_kind="business")
        return await self._dispatch(action, params, session)

    async def _do_open(self, owner: str, namespace: str = "") -> ToolResult:
        max_sessions = self._cfg_get("max_sessions", 3)
        max_total = self._cfg_get("max_total_sessions", 0)
        allowed, scope = self._mgr.check_limits(
            max_per_owner=max_sessions, max_total=max_total, owner=owner)
        if not allowed:
            # Name which ceiling was hit: "your quota is free but the pool is
            # full" is otherwise indistinguishable from a per-owner rejection,
            # and the model would keep retrying close/open on its own sessions.
            if scope == "total":
                return ToolResult(
                    success=False,
                    error=(f"浏览器会话总数已达全局上限({max_total})，"
                           "请稍后重试或先 close 一个会话"),
                    error_kind="business")
            return ToolResult(success=False,
                              error=f"会话已达上限({max_sessions})，请先 close 一个会话",
                              error_kind="business")
        try:
            sid = await self._mgr.open(
                headless=self._cfg_get("headless", True),
                allow_private=self._cfg_get("allow_private_addresses", False),
                owner=owner,
                dialog_policy=self._cfg_get("dialog_policy", "dismiss"),
                storage_state_path=self._state_path(owner, namespace),
                viewport_width=self._cfg_get("viewport_width", 1280),
                viewport_height=self._cfg_get("viewport_height", 800),
                user_agent=self._cfg_get("user_agent", ""),
            )
        except BrowserLaunchError as e:
            return ToolResult(success=False, error=str(e), error_kind="dependency")
        except Exception as e:
            return ToolResult(success=False, error=f"浏览器启动失败: {e}",
                              error_kind="dependency")
        return ToolResult(output=f"browser session opened: {sid}",
                          metadata={"session_id": sid})

    async def _snapshot(self, session: Any) -> str:
        return await _actions.refresh_snapshot(
            session, max_chars=self._cfg_get("max_snapshot_chars", 8000))

    # Ceiling for a caller-supplied timeout. timeout_seconds (90) bounds the whole
    # tool call, so letting the model pass 3600 would just hang the call until the
    # outer timeout killed it, with no snapshot and no usable error.
    _MAX_ACTION_TIMEOUT_SEC = 120

    def _action_timeout(self, params: dict[str, Any], default: int) -> int:
        """Resolve the effective timeout for one action.

        ``timeout_sec`` is advertised in the schema as overriding wait/navigate,
        but navigation used to ignore it and always take the global
        nav_timeout_sec — so a caller could neither extend a slow page nor cut a
        doomed wait short.
        """
        raw = params.get("timeout_sec")
        if raw in (None, ""):
            return default
        try:
            value = int(raw)
        except (TypeError, ValueError):
            return default
        if value <= 0:
            return default
        return min(value, self._MAX_ACTION_TIMEOUT_SEC)

    async def _dispatch(self, action: str, params: dict[str, Any],
                        session: Any) -> ToolResult:
        nav_timeout = self._action_timeout(params, self._cfg_get("nav_timeout_sec", 30))
        err = ""

        if action == "navigate":
            url = params.get("url", "")
            if not url:
                return ToolResult(success=False, error="navigate 需要 url",
                                  error_kind="validation")
            err = await _actions.navigate(
                session, url, timeout_sec=nav_timeout,
                allow_private=self._cfg_get("allow_private_addresses", False))
        elif action == "snapshot":
            pass
        elif action == "click":
            err = await _actions.click(session, params.get("ref", ""))
        elif action == "type":
            ref = params.get("ref", "")
            if not ref:
                return ToolResult(success=False, error="type 需要 ref",
                                  error_kind="validation")
            err = await _actions.type_text(
                session, ref, params.get("text", ""),
                press_enter=bool(params.get("press_enter", False)))
        elif action == "press":
            err = await _actions.press_key(session, params.get("key", ""),
                                           ref=params.get("ref", ""))
        elif action == "scroll":
            err = await _actions.scroll(session, params.get("direction", "down"),
                                        amount=int(params.get("amount") or 0))
        elif action == "back":
            err = await _actions.go_back(session, timeout_sec=nav_timeout)
        elif action == "forward":
            err = await _actions.go_forward(session, timeout_sec=nav_timeout)
        elif action == "reload":
            err = await _actions.reload(session, timeout_sec=nav_timeout)
        elif action == "hover":
            err = await _actions.hover(session, params.get("ref", ""))
        elif action == "select":
            values = params.get("values") or ([params["text"]] if params.get("text") else [])
            err = await _actions.select_option(session, params.get("ref", ""), values)
        elif action == "upload":
            err = await _actions.upload_files(session, params.get("ref", ""),
                                              params.get("paths") or [])
        elif action == "wait":
            err = await _actions.wait_for(
                session, text=params.get("text", ""), state=params.get("state", ""),
                timeout_sec=self._action_timeout(params, 15))
        elif action == "evaluate":
            return await self._do_evaluate(params, session)
        elif action == "console":
            return self._do_console(session)
        elif action == "screenshot":
            return await self._do_screenshot(params, session)
        elif action == "get_images":
            return await self._do_get_images(session)
        else:  # pragma: no cover - _KNOWN_ACTIONS is the gate
            return ToolResult(success=False, error=f"Unknown action: {action}",
                              error_kind="validation")

        if err:
            # A failed interaction still leaves the page in whatever state it
            # reached, so hand back a fresh snapshot with the error — otherwise
            # the model retries against refs that may no longer exist.
            kind = "business" if "ref " in err or "无效" in err else "dependency"
            snapshot = ""
            if action in _SNAPSHOT_AFTER:
                try:
                    snapshot = await self._snapshot(session)
                except Exception as e:
                    logger.debug("post-error snapshot failed: {}", e)
            error = f"{err}\n\n--- 当前页面 ---\n{snapshot}" if snapshot else err
            return ToolResult(success=False, error=error, error_kind=kind)

        text = await self._snapshot(session)
        return ToolResult(output=text)

    async def _do_evaluate(self, params: dict[str, Any], session: Any) -> ToolResult:
        # Hard gate, checked before the expression filter: the filter is a
        # best-effort blacklist over arbitrary JS, so a deployment that cannot
        # accept in-page code execution at all needs a switch that does not
        # depend on out-guessing an attacker's string.
        if not self._cfg_get("allow_evaluate", True):
            return ToolResult(
                success=False,
                error=("evaluate 已被禁用（browser.allow_evaluate=false）。"
                       "请改用 snapshot / scroll / get_images 获取页面内容"),
                error_kind="business")
        result, err = await _actions.evaluate(
            session, params.get("expression", ""),
            allow_unsafe=self._cfg_get("allow_unsafe_evaluate", False))
        if err:
            kind = "business" if "拒绝" in err or "不能为空" in err else "dependency"
            return ToolResult(success=False, error=err, error_kind=kind)
        return ToolResult(output=result)

    def _do_console(self, session: Any) -> ToolResult:
        lines = list(session.console)
        session.console.clear()
        if not lines:
            return ToolResult(output="(无控制台错误或警告)")
        return ToolResult(output="\n".join(lines),
                          metadata={"console_count": len(lines)})

    async def _do_screenshot(self, params: dict[str, Any], session: Any) -> ToolResult:
        data = await _actions.screenshot(
            session, full_page=bool(params.get("full_page", False)))
        if not data:
            return ToolResult(success=False, error="截图失败",
                              error_kind="dependency")
        try:
            out_dir = self._screenshot_dir()
            out_dir.mkdir(parents=True, exist_ok=True)
            path = out_dir / f"shot_{int(time.time())}_{uuid.uuid4().hex[:6]}.png"
            path.write_bytes(data)
        except Exception as e:
            return ToolResult(success=False, error=f"截图保存失败: {e}",
                              error_kind="internal")
        return ToolResult(
            output=(f"截图已保存: {path}\n"
                    f"({len(data)} bytes) 可用 vision_analyze 传入该路径进行视觉分析，"
                    "或用 send_file 发给用户。"),
            metadata={"screenshot_path": str(path), "image_bytes": len(data)},
        )

    async def _do_get_images(self, session: Any) -> ToolResult:
        rows = await _actions.get_images(session)
        if not rows:
            return ToolResult(output="(页面无可用图片)")
        lines = [
            f"{i + 1}. {r.get('url', '')} ({r.get('w')}x{r.get('h')})"
            + (f" alt={r.get('alt')!r}" if r.get("alt") else "")
            for i, r in enumerate(rows)
        ]
        return ToolResult(output="\n".join(lines), metadata={"image_count": len(rows)})
