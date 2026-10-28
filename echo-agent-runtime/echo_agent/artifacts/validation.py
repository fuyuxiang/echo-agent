"""Deterministic validation and metrics for user-facing text artifacts."""

from __future__ import annotations

import csv
import hashlib
import io
import json
import re
from pathlib import Path
from typing import Any

_CJK_RE = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]")
_WORD_RE = re.compile(r"[A-Za-z0-9]+(?:['’-][A-Za-z0-9]+)*")
_HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$", re.MULTILINE)


def validate_text(content: str, extension: str) -> dict[str, Any]:
    """Return stable format checks plus language-aware document metrics."""
    ext = extension.lower()
    lines = content.splitlines()
    paragraphs = [p for p in re.split(r"\n\s*\n", content.strip()) if p.strip()]
    headings = [
        {"level": len(match.group(1)), "text": match.group(2).strip()}
        for match in _HEADING_RE.finditer(content)
    ]
    errors: list[dict[str, Any]] = []
    warnings: list[dict[str, Any]] = []

    if not content.strip():
        errors.append({"code": "EMPTY_CONTENT", "message": "Artifact content is empty"})

    if ext == ".md":
        if content.count("```") % 2:
            errors.append({"code": "MD_UNCLOSED_FENCE", "message": "Markdown code fence is not closed"})
        previous = 0
        for heading in headings:
            level = int(heading["level"])
            if previous and level > previous + 1:
                warnings.append({
                    "code": "MD_HEADING_JUMP",
                    "message": f"Heading level jumps from H{previous} to H{level}",
                })
            previous = level
    elif ext == ".json":
        try:
            json.loads(content)
        except json.JSONDecodeError as exc:
            errors.append({
                "code": "JSON_INVALID", "line": exc.lineno, "column": exc.colno,
                "message": exc.msg,
            })
    elif ext == ".csv":
        try:
            rows = list(csv.reader(io.StringIO(content)))
            widths = {len(row) for row in rows if row}
            if len(widths) > 1:
                errors.append({"code": "CSV_RAGGED", "message": "CSV rows have inconsistent column counts"})
        except csv.Error as exc:
            errors.append({"code": "CSV_INVALID", "message": str(exc)})

    encoded = content.encode("utf-8")
    return {
        "valid": not errors,
        "format": ext.lstrip("."),
        "sha256": hashlib.sha256(encoded).hexdigest(),
        "bytes": len(encoded),
        "characters": len(content),
        "non_whitespace_characters": sum(not char.isspace() for char in content),
        "cjk_characters": len(_CJK_RE.findall(content)),
        "latin_words": len(_WORD_RE.findall(content)),
        "lines": len(lines),
        "paragraphs": len(paragraphs),
        "headings": headings,
        "errors": errors,
        "warnings": warnings,
    }


def validate_path(path: Path) -> dict[str, Any]:
    return validate_text(path.read_text(encoding="utf-8"), path.suffix)
