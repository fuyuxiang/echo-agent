"""Text utilities — message splitting, token estimation, markdown stripping."""

from __future__ import annotations

import re


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


def strip_thinking(text: str) -> str:
    """Remove <think>...</think> blocks and orphaned thinking tags from LLM output."""
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL)
    text = re.sub(r"</?think>", "", text, flags=re.IGNORECASE)
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


# ─── Markdown normalization for restricted channels (QQ Bot, SMS, …) ─────────
# QQ Bot's msg_type=2 markdown only renders a small subset (bold/italic/inline
# code/code-fence/link); tables, headings, HR and lists are NOT supported. And
# msg_type=0 plain text renders every markdown marker as literal source noise.
# normalize_markdown() downgrades GFM into a form the target channel can show:
#   keep_inline=False → strip all inline markers (plain-text channels)
#   keep_inline=True  → keep QQ-supported inline markers, only downgrade the
#                       block-level constructs QQ can't render (table/heading/HR)
# Block-level handling (table→fields, heading→text, HR→removed) is shared by
# both modes because no channel here renders those.

_RE_MD_BOLD = re.compile(r"\*\*(.+?)\*\*", re.DOTALL)
_RE_MD_ITALIC_STAR = re.compile(r"\*(.+?)\*", re.DOTALL)
_RE_MD_BOLD_UNDER = re.compile(r"\b__(?![\s_])(.+?)(?<![\s_])__\b", re.DOTALL)
_RE_MD_ITALIC_UNDER = re.compile(r"\b_(?![\s_])(.+?)(?<![\s_])_\b", re.DOTALL)
_RE_MD_INLINE_CODE = re.compile(r"`(.+?)`")
_RE_MD_LINK = re.compile(r"\[([^\]]+)\]\([^\)]+\)")
_RE_MD_HEADING = re.compile(r"^\s{0,3}#{1,6}\s+")
_RE_MD_HR = re.compile(r"^\s*([-*_])(?:\s*\1){2,}\s*$")
_RE_MD_FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
_RE_MD_SEPARATOR_CELL = re.compile(r"^:?-+:?$")


def _split_table_cells(line: str) -> list[str]:
    """Split a GFM table row into trimmed cells.

    A backslash-escaped pipe (``\\|``) is literal content per GFM, not a column
    delimiter; ``\\\\`` unescapes to a single backslash. Leading/trailing pipes
    are dropped before splitting.
    """
    inner = line.strip()
    if inner.startswith("|"):
        inner = inner[1:]
    if inner.endswith("|"):
        inner = inner[:-1]
    cells: list[str] = []
    current = ""
    i = 0
    while i < len(inner):
        char = inner[i]
        if char == "\\" and i + 1 < len(inner):
            nxt = inner[i + 1]
            current += nxt if nxt in ("|", "\\") else f"\\{nxt}"
            i += 2
            continue
        if char == "|":
            cells.append(current.strip())
            current = ""
            i += 1
            continue
        current += char
        i += 1
    cells.append(current.strip())
    return cells


def _is_table_row(line: str) -> bool:
    trimmed = line.strip()
    # A single leading+trailing pipe wrapping >=1 cell. Real tables are
    # confirmed by a following separator row, so a lone ``|x|`` text line that
    # never gets one is flushed back verbatim rather than mangled.
    return (
        trimmed.startswith("|")
        and trimmed.endswith("|")
        and len(_split_table_cells(trimmed)) >= 1
    )


def _is_table_separator(line: str) -> bool:
    if not _is_table_row(line):
        return False
    cells = _split_table_cells(line)
    return bool(cells) and all(_RE_MD_SEPARATOR_CELL.match(c.strip()) for c in cells)


def _render_table_row_as_fields(headers: list[str], cells: list[str]) -> str:
    """Render one table row as ``header: value`` lines."""
    parts = []
    for idx, cell in enumerate(cells):
        header = headers[idx].strip() if idx < len(headers) else ""
        parts.append(f"{header}: {cell}" if header else cell)
    return "\n".join(parts)


def _strip_inline_markers(text: str) -> str:
    """Remove inline markdown markers, keeping the visible text."""
    text = _RE_MD_BOLD.sub(r"\1", text)
    text = _RE_MD_BOLD_UNDER.sub(r"\1", text)
    text = _RE_MD_ITALIC_STAR.sub(r"\1", text)
    text = _RE_MD_ITALIC_UNDER.sub(r"\1", text)
    text = _RE_MD_INLINE_CODE.sub(r"\1", text)
    text = _RE_MD_LINK.sub(r"\1", text)
    return text


def normalize_markdown(text: str, *, keep_inline: bool = False) -> str:
    """Downgrade GFM markdown into a form restricted channels can render.

    Block-level constructs unsupported everywhere here are always downgraded:
      - Tables → ``header: value`` field blocks (rows separated by blank lines).
      - Headings (``## x``) → plain ``x`` (leading hashes removed).
      - Horizontal rules (``---``) → dropped.
    Inline markers (bold/italic/inline-code/link) are removed when
    ``keep_inline`` is False (plain-text channels) and preserved when True
    (channels whose markdown renders that subset, e.g. QQ msg_type=2).
    Content inside fenced code blocks is passed through untouched in both modes.
    """
    if not text:
        return text

    out: list[str] = []
    # Pending table state: header cells + buffered data rows.
    header_line: str | None = None
    header_cells: list[str] = []
    have_separator = False
    data_rows: list[list[str]] = []
    in_fence = False
    fence_marker = ""

    def flush_table() -> None:
        nonlocal header_line, header_cells, have_separator, data_rows
        if header_line is not None:
            if have_separator and data_rows:
                blocks = [
                    _render_table_row_as_fields(header_cells, row) for row in data_rows
                ]
                out.append(_line_out("\n\n".join(blocks)))
            else:
                # Header without a valid separator/rows: not a real table,
                # emit the original line so content is never silently dropped.
                out.append(_line_out(header_line))
        header_line = None
        header_cells = []
        have_separator = False
        data_rows = []

    def _line_out(line: str) -> str:
        return line if keep_inline else _strip_inline_markers(line)

    for line in text.split("\n"):
        fence_match = _RE_MD_FENCE.match(line)
        if fence_match:
            marker = fence_match.group(1)
            if not in_fence:
                flush_table()
                in_fence = True
                fence_marker = marker[0]
            elif marker[0] == fence_marker:
                in_fence = False
                fence_marker = ""
            out.append(line)
            continue
        if in_fence:
            out.append(line)
            continue

        # Inside a pending table: separator, data row, or table end.
        if header_line is not None:
            if not have_separator and _is_table_separator(line):
                have_separator = True
                continue
            if have_separator and _is_table_row(line) and not _is_table_separator(line):
                data_rows.append(_split_table_cells(line))
                continue
            flush_table()

        if _is_table_row(line) and not _is_table_separator(line):
            header_line = line
            header_cells = _split_table_cells(line)
            continue

        if _RE_MD_HR.match(line):
            continue
        heading = _RE_MD_HEADING.match(line)
        if heading:
            line = line[heading.end():]
        out.append(_line_out(line))

    flush_table()
    result = "\n".join(out)
    result = re.sub(r"\n{3,}", "\n\n", result)
    return result.strip()
