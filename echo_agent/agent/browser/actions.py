from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.agent.browser.snapshot import build_snapshot_with_locators
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
    if not allow_private:
        # Re-check the FINAL url after all redirects. The browser follows 30x
        # hops itself and resolves DNS on its own, so the initial check does
        # not cover redirect-to-internal or DNS-rebinding. This is the hard
        # guard against a public URL bouncing to 169.254.169.254 etc.
        final_url = getattr(session.page, "url", "") or url
        final_error = await check_url_ssrf(final_url)
        if final_error:
            return ("navigation blocked: redirected to non-public address "
                    f"({final_error})")
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

    build_snapshot_with_locators yields (text, {ref: locator}) from a single
    AX-tree traversal, so the @eN in the text and the locator that click/type
    will drive are guaranteed to come from the same node. No separate DOM query
    is used, which is what previously let radio/menuitem-style roles shift refs.
    """
    text, ref_map = await build_snapshot_with_locators(
        session.page, max_chars=max_chars
    )
    session.ref_map = ref_map
    return text
