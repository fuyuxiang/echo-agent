from __future__ import annotations

import time
import urllib.parse
import uuid
from dataclasses import dataclass, field
from typing import Any

from loguru import logger

from echo_agent.agent.tools.web import check_url_ssrf

_SSRF_CACHE_TTL_SEC = 30.0


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


class BrowserSessionManager:
    def __init__(self) -> None:
        self._pw: Any = None
        self._browser: Any = None
        self._sessions: dict[str, BrowserSession] = {}

    async def _ensure_browser(self, headless: bool) -> None:
        if self._pw is None:
            self._pw = await _start_playwright()
            self._browser = await self._pw.chromium.launch(headless=headless)

    async def open(self, *, headless: bool = True, allow_private: bool = False) -> str:
        await self._ensure_browser(headless)
        context = await self._browser.new_context()
        page = await context.new_page()
        sid = f"sess_{uuid.uuid4().hex[:8]}"
        session = BrowserSession(context=context, page=page,
                                 last_active=time.monotonic(),
                                 allow_private=allow_private)
        await context.route("**/*", lambda route: _ssrf_route_handler(session, route))
        self._sessions[sid] = session
        return sid

    def get(self, session_id: str) -> BrowserSession | None:
        s = self._sessions.get(session_id)
        if s is not None:
            s.last_active = time.monotonic()
        return s

    def get_count(self) -> int:
        return len(self._sessions)

    async def close(self, session_id: str) -> bool:
        s = self._sessions.pop(session_id, None)
        if s is None:
            return False
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

    def _enforce_limit(self, max_sessions: int) -> bool:
        """True if there is room for another session."""
        return len(self._sessions) < max_sessions


manager = BrowserSessionManager()
