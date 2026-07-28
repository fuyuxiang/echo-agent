"""Browser session lifecycle: launch, per-owner isolation, SSRF gating, reaping."""

from __future__ import annotations

import asyncio
import time
import urllib.parse
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.tools.web import check_url_ssrf

_SSRF_CACHE_TTL_SEC = 30.0
# Native dialogs (alert/confirm/prompt/beforeunload) block the renderer until
# answered. Playwright auto-dismisses only when no handler is registered, and a
# handler is required to record what the page asked. We answer per policy and
# keep the text so the model can see it in the next snapshot.
_MAX_RECORDED_DIALOGS = 10
_MAX_CONSOLE_MESSAGES = 200


class BrowserLaunchError(RuntimeError):
    """Raised when Chromium cannot be launched, carrying an actionable hint."""


async def _start_playwright() -> Any:
    """Bootstrap Playwright; isolated for test monkeypatching."""
    from playwright.async_api import async_playwright
    return await async_playwright().start()


@dataclass
class BrowserSession:
    context: Any
    page: Any
    last_active: float
    ref_map: dict[str, Any] = field(default_factory=dict)
    allow_private: bool = False
    ssrf_cache: dict[str, tuple[bool, float]] = field(default_factory=dict)
    # Owner key (session_key/user_id of the caller that opened it). Every action
    # re-checks this so one conversation cannot drive another's logged-in pages
    # just by guessing or leaking a session id.
    owner: str = ""
    dialog_policy: str = "dismiss"
    dialogs: list[dict[str, str]] = field(default_factory=list)
    console: list[str] = field(default_factory=list)
    storage_state_path: str = ""

    def record_dialog(self, entry: dict[str, str]) -> None:
        self.dialogs.append(entry)
        if len(self.dialogs) > _MAX_RECORDED_DIALOGS:
            del self.dialogs[:-_MAX_RECORDED_DIALOGS]

    def record_console(self, line: str) -> None:
        self.console.append(line)
        if len(self.console) > _MAX_CONSOLE_MESSAGES:
            del self.console[:-_MAX_CONSOLE_MESSAGES]

    def take_dialogs(self) -> list[dict[str, str]]:
        out = list(self.dialogs)
        self.dialogs.clear()
        return out


async def _ssrf_route_handler(session: BrowserSession, route: Any) -> None:
    """Per-request SSRF gate: validate the target host BEFORE the request is
    sent, covering the main navigation, every redirect hop, and all subresources
    (img/xhr/fetch). Fails safe — any validation error aborts rather than allows.
    """
    if session.allow_private:
        await route.continue_()
        return
    url = route.request.url
    # Non-network schemes (data:/blob:/about:) are inline payloads or in-page
    # object references, not SSRF vectors. resolve_and_validate rejects any
    # non-http(s) scheme, so gating them here keeps inline images/media/workers
    # from being wrongly aborted and breaking page rendering.
    if urllib.parse.urlsplit(url).scheme not in ("http", "https"):
        await route.continue_()
        return
    host = urllib.parse.urlsplit(url).hostname or url
    now = time.monotonic()
    cached = session.ssrf_cache.get(host)
    if cached is not None and cached[1] > now:
        ok = cached[0]
    else:
        try:
            error = await check_url_ssrf(url)
            ok = error is None
        except Exception as e:  # fail-safe: block on validation failure
            logger.warning("SSRF check errored, blocking {} : {}", url, e)
            ok = False
        session.ssrf_cache[host] = (ok, now + _SSRF_CACHE_TTL_SEC)
    if ok:
        await route.continue_()
    else:
        logger.warning("browser request blocked (SSRF guard): {}", url)
        await route.abort("blockedbyclient")


async def _dialog_handler(session: BrowserSession, dialog: Any) -> None:
    """Answer a native dialog per policy and record it for the model.

    Without a handler an ``alert()`` leaves the page blocked until every later
    action times out, which is indistinguishable from a hung site. Recording the
    message means the model learns *why* the page changed.
    """
    try:
        kind = dialog.type
        message = dialog.message
    except Exception:  # pragma: no cover - defensive
        kind, message = "unknown", ""
    session.record_dialog({"type": str(kind), "message": str(message)})
    try:
        if session.dialog_policy == "accept":
            await dialog.accept()
        else:
            await dialog.dismiss()
    except Exception as e:
        # Already handled or page gone; nothing actionable left to do.
        logger.debug("dialog {} handling raised (ignored): {}", kind, e)


def _console_handler(session: BrowserSession, message: Any) -> None:
    try:
        kind = message.type
        text = message.text
    except Exception:  # pragma: no cover - defensive
        return
    if kind in ("error", "warning"):
        session.record_console(f"[{kind}] {text}"[:500])


def _page_error_handler(session: BrowserSession, error: Any) -> None:
    """Record an uncaught page exception.

    The context-level ``weberror`` event delivers a ``WebError`` wrapper whose
    ``str()`` is just an object repr; the useful message hangs off ``.error``.
    The page-level ``pageerror`` event passes the error directly. Handle both so
    the model sees the actual exception text either way.
    """
    payload = getattr(error, "error", error)
    text = str(getattr(payload, "message", "") or payload)
    if not text or text.startswith("<"):
        text = repr(payload)
    name = str(getattr(payload, "name", "") or "")
    if name and not text.startswith(name):
        text = f"{name}: {text}"
    session.record_console(f"[pageerror] {text}"[:500])


class BrowserSessionManager:
    def __init__(self) -> None:
        self._pw: Any = None
        self._browser: Any = None
        self._sessions: dict[str, BrowserSession] = {}
        # Serialize browser bootstrap: two concurrent open() calls would each
        # launch Chromium and the loser's process would leak.
        self._launch_lock = asyncio.Lock()

    async def _ensure_browser(self, headless: bool) -> None:
        async with self._launch_lock:
            if self._pw is not None and self._browser is not None:
                return
            if self._pw is None:
                self._pw = await _start_playwright()
            try:
                self._browser = await self._pw.chromium.launch(headless=headless)
            except Exception as e:
                text = str(e)
                if "Executable doesn't exist" in text or "playwright install" in text:
                    raise BrowserLaunchError(
                        "Chromium 未安装，请先执行: python -m playwright install chromium"
                    ) from e
                raise BrowserLaunchError(f"浏览器启动失败: {text}") from e

    async def open(
        self,
        *,
        headless: bool = True,
        allow_private: bool = False,
        owner: str = "",
        dialog_policy: str = "dismiss",
        storage_state_path: str = "",
        viewport_width: int = 1280,
        viewport_height: int = 800,
        user_agent: str = "",
    ) -> str:
        await self._ensure_browser(headless)
        ctx_kwargs: dict[str, Any] = {
            "viewport": {"width": viewport_width, "height": viewport_height},
        }
        if user_agent:
            ctx_kwargs["user_agent"] = user_agent
        # Reuse cookies/localStorage from a previous session so a login survives
        # across tasks. Missing/corrupt state must not block opening a browser.
        if storage_state_path and Path(storage_state_path).is_file():
            ctx_kwargs["storage_state"] = storage_state_path
        try:
            context = await self._browser.new_context(**ctx_kwargs)
        except Exception as e:
            if "storage_state" in ctx_kwargs:
                logger.warning("storage_state 载入失败，改用空白会话: {}", e)
                ctx_kwargs.pop("storage_state")
                context = await self._browser.new_context(**ctx_kwargs)
            else:
                raise
        page = await context.new_page()
        sid = f"sess_{uuid.uuid4().hex[:8]}"
        session = BrowserSession(context=context, page=page,
                                 last_active=time.monotonic(),
                                 allow_private=allow_private,
                                 owner=owner,
                                 dialog_policy=dialog_policy,
                                 storage_state_path=storage_state_path)
        await context.route("**/*", lambda route: _ssrf_route_handler(session, route))
        # Bind on the context, not the page: popups opened by the page inherit
        # these handlers, so a dialog in a popup can't hang the session either.
        try:
            context.on("dialog", lambda dialog: asyncio.ensure_future(
                _dialog_handler(session, dialog)))
            context.on("console", lambda msg: _console_handler(session, msg))
            context.on("weberror", lambda err: _page_error_handler(session, err))
        except Exception as e:
            # Older Playwright builds expose these on Page only; fall back so a
            # missing context-level event never blocks session creation.
            logger.debug("context event binding failed, falling back to page: {}", e)
            page.on("dialog", lambda dialog: asyncio.ensure_future(
                _dialog_handler(session, dialog)))
            page.on("console", lambda msg: _console_handler(session, msg))
            page.on("pageerror", lambda err: _page_error_handler(session, err))
        self._sessions[sid] = session
        return sid

    def get(self, session_id: str, owner: str = "") -> BrowserSession | None:
        """Look up a live session. When *owner* is non-empty it must match the
        session's owner, so a leaked/guessed session id from another
        conversation resolves to nothing instead of a live logged-in page."""
        s = self._sessions.get(session_id)
        if s is None:
            return None
        if owner and s.owner and s.owner != owner:
            logger.warning("browser session {} owner mismatch, denying access", session_id)
            return None
        s.last_active = time.monotonic()
        return s

    def get_count(self, owner: str = "") -> int:
        if not owner:
            return len(self._sessions)
        return sum(1 for s in self._sessions.values() if s.owner == owner)

    async def save_storage_state(self, session_id: str, owner: str = "") -> str:
        """Persist cookies/localStorage of a session to its configured path."""
        s = self.get(session_id, owner)
        if s is None or not s.storage_state_path:
            return ""
        try:
            Path(s.storage_state_path).parent.mkdir(parents=True, exist_ok=True)
            await s.context.storage_state(path=s.storage_state_path)
            return s.storage_state_path
        except Exception as e:
            logger.warning("storage_state 保存失败: {}", e)
            return ""

    async def close(self, session_id: str, owner: str = "") -> bool:
        s = self._sessions.get(session_id)
        if s is None:
            return False
        if owner and s.owner and s.owner != owner:
            logger.warning("browser session {} owner mismatch, refusing close", session_id)
            return False
        # Flush the login state before tearing the context down, otherwise a
        # session that authenticated has nothing to hand the next task.
        if s.storage_state_path:
            try:
                Path(s.storage_state_path).parent.mkdir(parents=True, exist_ok=True)
                await s.context.storage_state(path=s.storage_state_path)
            except Exception as e:
                logger.debug("storage_state flush on close failed (ignored): {}", e)
        self._sessions.pop(session_id, None)
        try:
            await s.context.close()
        except Exception as e:
            logger.debug("browser context close raised (ignored): {}", e)
        return True

    async def close_all(self) -> None:
        for sid in list(self._sessions.keys()):
            await self.close(sid)
        # Tear down the shared browser process and playwright driver too;
        # closing only contexts leaked the Chromium/driver processes on every
        # in-process restart. Reset to None so the next open() re-bootstraps.
        if self._browser is not None:
            try:
                await self._browser.close()
            except Exception as e:
                logger.debug("browser close raised (ignored): {}", e)
        if self._pw is not None:
            try:
                await self._pw.stop()
            except Exception as e:
                logger.debug("playwright stop raised (ignored): {}", e)
        self._browser = None
        self._pw = None

    async def _reap_idle(self, idle_timeout_sec: float) -> None:
        now = time.monotonic()
        stale = [sid for sid, s in self._sessions.items()
                 if now - s.last_active > idle_timeout_sec]
        for sid in stale:
            logger.debug("reaping idle browser session {}", sid)
            await self.close(sid)

    def check_limits(self, *, max_per_owner: int, max_total: int = 0,
                     owner: str = "") -> tuple[bool, str]:
        """Whether another session may be opened, and which ceiling refused it.

        Two independent ceilings, both of which must pass:

        * per owner — a single busy conversation must not exhaust the pool and
          lock everyone else out.
        * global — every owner being under its own limit said nothing about the
          total, so N conversations could each launch ``max_per_owner`` Chromium
          contexts with no bound on the sum. Each context costs real memory, so
          the machine, not just the conversation, needs a cap.

        ``max_total <= 0`` disables the global ceiling.
        """
        if self.get_count(owner) >= max_per_owner:
            return False, "owner"
        if max_total > 0 and len(self._sessions) >= max_total:
            return False, "total"
        return True, ""

    def _enforce_limit(self, max_sessions: int, owner: str = "") -> bool:
        """Per-owner room check. Retained for callers that predate check_limits."""
        return self.check_limits(max_per_owner=max_sessions, owner=owner)[0]


manager = BrowserSessionManager()
