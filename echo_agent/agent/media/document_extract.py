"""Document text extraction core — pure local-path → text, no network/decrypt/model.

Shared by inbound auto-injection (agent/context.py) and the read_document tool.
Every branch degrades safely: missing deps, corrupt files, and unsupported
formats return an empty ExtractResult with a meta marker, never raise."""
from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from loguru import logger

_TEXT_EXTS = {".txt", ".md", ".csv", ".json", ".yaml", ".yml", ".log", ".xml", ".html"}


@dataclass
class ExtractResult:
    text: str = ""
    meta: dict[str, Any] = field(default_factory=dict)
    truncated: bool = False
    unit_count: int = 0


def _apply_cap(text: str, max_chars: int | None) -> tuple[str, bool]:
    if max_chars is not None and max_chars >= 0 and len(text) > max_chars:
        return text[:max_chars], True
    return text, False


def extract(
    path: str | Path,
    *,
    max_chars: int | None = None,
    unit: int | str | None = None,
) -> ExtractResult:
    p = Path(path)
    if not p.exists() or not p.is_file():
        return ExtractResult(meta={"format": "missing", "error": f"file not found: {p}"})
    ext = p.suffix.lower()
    size = p.stat().st_size
    try:
        if ext == ".pdf":
            return _extract_pdf(p, max_chars, unit, size)
        if ext in _TEXT_EXTS:
            return _extract_text(p, max_chars, size)
    except Exception as exc:  # noqa: BLE001 — inbound path must never raise
        logger.warning("document extract failed for {}: {}", p.name, exc)
        return ExtractResult(meta={"format": ext.lstrip("."), "size_bytes": size, "error": str(exc)})
    return ExtractResult(meta={"format": "unsupported", "size_bytes": size})


def _extract_text(p: Path, max_chars: int | None, size: int) -> ExtractResult:
    raw = p.read_text(encoding="utf-8", errors="replace")
    text, truncated = _apply_cap(raw, max_chars)
    return ExtractResult(
        text=text,
        meta={"format": p.suffix.lower().lstrip("."), "size_bytes": size},
        truncated=truncated,
        unit_count=raw.count("\n") + 1,
    )


def _extract_pdf(p: Path, max_chars: int | None, unit: int | str | None, size: int) -> ExtractResult:
    try:
        import pymupdf  # type: ignore
    except ImportError:
        return ExtractResult(meta={"format": "pdf", "size_bytes": size,
                                   "error": "missing_dep:pymupdf>=1.24"})
    doc = pymupdf.open(str(p))
    try:
        total = doc.page_count
        parts: list[str] = []
        target_page = int(unit) if isinstance(unit, (int, str)) and str(unit).isdigit() else None
        for i in range(total):
            if target_page is not None and (i + 1) != target_page:
                continue
            page_text = doc[i].get_text()
            parts.append(f"--- 第{i + 1}页 ---\n{page_text}")
            if max_chars is not None and sum(len(x) for x in parts) >= max_chars:
                break
        joined = "\n".join(parts)
        text, truncated = _apply_cap(joined, max_chars)
        return ExtractResult(text=text, meta={"format": "pdf", "size_bytes": size, "pages": total},
                             truncated=truncated, unit_count=total)
    finally:
        doc.close()
