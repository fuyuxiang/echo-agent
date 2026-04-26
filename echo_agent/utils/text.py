"""Text utilities — message splitting, token estimation, markdown stripping."""

from __future__ import annotations

import re
from typing import Any


def split_message(text: str, max_len: int = 2000, *, min_chunk_ratio: float = 0.75) -> list[str]:
    """Split a long message into as few chunks as possible.

    Natural boundaries are preferred only when they are close enough to the
    platform limit; early newlines should not turn one long answer into many
    short messages.
    """
    if len(text) <= max_len:
        return [text]
    min_cut = max(1, min(max_len - 1, int(max_len * min_chunk_ratio)))
    chunks: list[str] = []
    while text:
        if len(text) <= max_len:
            chunks.append(text)
            break
        cut = _best_split_position(text, max_len, min_cut)
        chunk = text[:cut].rstrip()
        if not chunk:
            chunk = text[:cut]
        chunks.append(chunk)
        text = text[cut:].lstrip()
    return chunks


def _best_split_position(text: str, max_len: int, min_cut: int) -> int:
    boundary_groups = (
        ("\n\n",),
        ("\n",),
        ("\u3002", "\uff01", "\uff1f", ".", "!", "?"),
        ("\uff1b", ";"),
        ("\uff0c", ","),
        (" ",),
    )
    for boundaries in boundary_groups:
        cut = _last_boundary_cut(text, boundaries, max_len, min_cut)
        if cut:
            return cut
    return max_len


def _last_boundary_cut(text: str, boundaries: tuple[str, ...], max_len: int, min_cut: int) -> int:
    best = 0
    for boundary in boundaries:
        start = 0
        while True:
            idx = text.find(boundary, start, max_len)
            if idx < 0:
                break
            candidate = idx + len(boundary)
            if min_cut <= candidate <= max_len:
                best = max(best, candidate)
            start = idx + len(boundary)
    return best


def estimate_tokens(text: str) -> int:
    """Rough token estimate (~4 chars per token for English, ~2 for CJK)."""
    if not text:
        return 0
    ascii_chars = sum(1 for c in text if ord(c) < 128)
    non_ascii = len(text) - ascii_chars
    return ascii_chars // 4 + non_ascii // 2 + 1


def estimate_messages_tokens(messages: list[dict[str, Any]]) -> int:
    """Estimate total tokens for a list of chat messages."""
    total = 0
    for msg in messages:
        content = msg.get("content", "")
        if isinstance(content, str):
            total += estimate_tokens(content) + 4
        elif isinstance(content, list):
            for block in content:
                total += estimate_tokens(str(block.get("text", ""))) + 4
        tool_calls = msg.get("tool_calls", [])
        for tc in tool_calls:
            fn = tc.get("function", {})
            total += estimate_tokens(fn.get("name", "")) + estimate_tokens(fn.get("arguments", "")) + 4
    return total


def strip_thinking(text: str) -> str:
    """Remove <think>...</think> blocks from LLM output."""
    return re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL).strip()


def strip_markdown(text: str) -> str:
    """Convert markdown to plain text (basic)."""
    text = re.sub(r"!\[([^\]]*)\]\([^)]+\)", r"\1", text)
    text = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", text)
    text = re.sub(r"[*_]{1,3}(.+?)[*_]{1,3}", r"\1", text)
    text = re.sub(r"`{1,3}[^`]*`{1,3}", lambda m: m.group().strip("`"), text)
    text = re.sub(r"^#{1,6}\s+", "", text, flags=re.MULTILINE)
    return text.strip()


def html_to_text(html: str) -> str:
    """Basic HTML to plain text conversion."""
    text = re.sub(r"<br\s*/?>", "\n", html, flags=re.IGNORECASE)
    text = re.sub(r"<p[^>]*>", "\n", text, flags=re.IGNORECASE)
    text = re.sub(r"</p>", "", text, flags=re.IGNORECASE)
    text = re.sub(r"<[^>]+>", "", text)
    text = re.sub(r"&nbsp;", " ", text)
    text = re.sub(r"&amp;", "&", text)
    text = re.sub(r"&lt;", "<", text)
    text = re.sub(r"&gt;", ">", text)
    text = re.sub(r"&quot;", '"', text)
    text = re.sub(r"&#39;", "'", text)
    return text.strip()
