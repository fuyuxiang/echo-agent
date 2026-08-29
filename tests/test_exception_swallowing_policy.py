"""Architecture guard for intentionally swallowed exceptions.

An empty ``except`` is occasionally the correct fail-open/fail-closed boundary,
but it must be reviewable in place. A handler containing ``pass`` therefore has
to log the exception or carry a nearby comment explaining why silence is safe.
"""

from __future__ import annotations

import ast
import re
import tokenize
from io import StringIO
from pathlib import Path


_ROOT = Path(__file__).parents[1] / "echo_agent"
_LOG_METHODS = {"debug", "info", "warning", "error", "exception", "critical", "log"}
_DIRECTIVE_RE = re.compile(
    r"^(?:noqa\b|type:\s*ignore\b|pyright:\s*ignore\b|"
    r"pragma:\s*no\s*cover\b|coverage:)",
    re.IGNORECASE,
)


def _comments_by_line(source: str) -> dict[int, str]:
    return {
        token.start[0]: token.string.removeprefix("#").strip()
        for token in tokenize.generate_tokens(StringIO(source).readline)
        if token.type == tokenize.COMMENT
    }


def _logs_exception(handler: ast.ExceptHandler) -> bool:
    for node in ast.walk(handler):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Attribute) and func.attr in _LOG_METHODS:
            return True
    return False


def _is_explanatory_comment(comment: str) -> bool:
    """Reject bare linter/type/coverage directives as safety rationale."""
    text = comment.strip()
    if not text:
        return False
    if not _DIRECTIVE_RE.match(text):
        return True
    # A directive may carry a human rationale after an explicit separator,
    # e.g. ``noqa: BLE001 — cleanup must be best-effort``.
    for separator in (" — ", " - ", "; "):
        if separator in text and text.split(separator, 1)[1].strip():
            return True
    return False


def test_every_pass_in_exception_handler_is_explained_or_logged() -> None:
    unexplained: list[str] = []
    for path in sorted(_ROOT.rglob("*.py")):
        source = path.read_text(encoding="utf-8")
        comments_by_line = _comments_by_line(source)
        tree = ast.parse(source, filename=str(path))
        for handler in (node for node in ast.walk(tree) if isinstance(node, ast.ExceptHandler)):
            if not any(isinstance(node, ast.Pass) for node in ast.walk(handler)):
                continue
            if _logs_exception(handler):
                continue
            # Include the two lines immediately before ``except`` because the
            # reason commonly documents the whole best-effort try/except pair.
            start = max(0, handler.lineno - 3)
            end = handler.end_lineno or handler.lineno
            # Token line numbers are one-based; ``start``/``end`` above are
            # zero-/one-based respectively and deliberately cover that range.
            comments = [
                comment
                for line_number, comment in comments_by_line.items()
                if start < line_number <= end and _is_explanatory_comment(comment)
            ]
            if comments:
                continue
            relative = path.relative_to(_ROOT.parent)
            unexplained.append(f"{relative}:{handler.lineno}")

    assert unexplained == [], (
        "silent exception handler(s) need an explanatory comment or a log call: "
        + ", ".join(unexplained)
    )


def test_tooling_directives_do_not_count_as_exception_rationale() -> None:
    assert not _is_explanatory_comment("noqa: BLE001")
    assert not _is_explanatory_comment("type: ignore[union-attr]")
    assert not _is_explanatory_comment("pragma: no cover")
    assert _is_explanatory_comment(
        "noqa: BLE001 — cleanup failure must not mask the original error"
    )
