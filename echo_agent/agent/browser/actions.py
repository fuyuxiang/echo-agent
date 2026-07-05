from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.agent.browser.snapshot import build_snapshot
from echo_agent.agent.tools.web import check_url_ssrf


async def navigate(session: Any, url: str, *, timeout_sec: int = 30,
                   allow_private: bool = False) -> str:
    if not allow_private:
        error = await check_url_ssrf(url)
        if error:
            return f"navigation blocked: {error}"
    try:
        await session.page.goto(url, timeout=timeout_sec * 1000)
    except Exception as e:
        return f"navigation failed: {e}"
    return ""


async def click(session: Any, ref: str) -> str:
    loc = session.ref_map.get(ref)
    if loc is None:
        return f"ref {ref} 不存在，请重新 snapshot"
    try:
        await loc.click(timeout=10000)
    except Exception as e:
        return f"click failed: {e}"
    return ""


async def type_text(session: Any, ref: str, text: str) -> str:
    loc = session.ref_map.get(ref)
    if loc is None:
        return f"ref {ref} 不存在，请重新 snapshot"
    try:
        await loc.fill(text, timeout=10000)
    except Exception as e:
        return f"type failed: {e}"
    return ""


async def screenshot(session: Any) -> bytes:
    try:
        return await session.page.screenshot()
    except Exception as e:
        logger.debug("screenshot failed: {}", e)
        return b""


async def refresh_snapshot(session: Any, *, max_chars: int = 8000) -> str:
    """Rebuild the snapshot text AND populate session.ref_map with locators.

    build_snapshot yields (text, {ref: node_index}); we map each ref to a
    Playwright locator via the page's ARIA-role query so click/type can act on
    it. The nth interactive node of role R is located by get_by_role(R).nth(k).
    """
    text, index_map = await build_snapshot(session.page, max_chars=max_chars)
    # index_map values are 1-based encounter order across all interactive nodes;
    # we resolve each to a locator by global interactive index using a flat query.
    ref_map: dict[str, Any] = {}
    try:
        # locate all interactive elements in DOM order to match snapshot order
        handles = await session.page.query_selector_all(
            "a, button, input, select, textarea, [role=button], [role=link], "
            "[role=textbox], [role=checkbox], [role=combobox], [role=tab]"
        )
        for ref, idx in index_map.items():
            pos = idx - 1
            if 0 <= pos < len(handles):
                ref_map[ref] = handles[pos]
    except Exception as e:
        logger.debug("ref locator mapping failed (refs will be stale): {}", e)
    session.ref_map = ref_map
    return text
