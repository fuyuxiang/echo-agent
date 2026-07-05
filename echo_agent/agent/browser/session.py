from __future__ import annotations

import time
import uuid
from dataclasses import dataclass, field
from typing import Any

from loguru import logger


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


class BrowserSessionManager:
    def __init__(self) -> None:
        self._pw: Any = None
        self._browser: Any = None
        self._sessions: dict[str, BrowserSession] = {}

    async def _ensure_browser(self, headless: bool) -> None:
        if self._pw is None:
            self._pw = await _start_playwright()
            self._browser = await self._pw.chromium.launch(headless=headless)

    async def open(self, *, headless: bool = True) -> str:
        await self._ensure_browser(headless)
        context = await self._browser.new_context()
        page = await context.new_page()
        sid = f"sess_{uuid.uuid4().hex[:8]}"
        self._sessions[sid] = BrowserSession(context=context, page=page,
                                             last_active=time.monotonic())
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
