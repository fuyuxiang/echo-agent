"""Checkers turn a written file into a list of Diagnostics.

First release ships Python only: PyCompileChecker (in-process syntax floor,
zero deps) + RuffChecker (semantic diagnostics when ruff is on PATH).
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Protocol, runtime_checkable

from loguru import logger


@dataclass
class Diagnostic:
    severity: str  # "error" | "warning"
    line: int
    col: int
    code: str
    message: str


@runtime_checkable
class Checker(Protocol):
    def can_check(self, path: Path) -> bool: ...
    async def check(self, path: Path) -> list[Diagnostic]: ...


class PyCompileChecker:
    """Syntax floor for .py files using the stdlib compiler. Always available."""

    def can_check(self, path: Path) -> bool:
        return path.suffix == ".py"

    async def check(self, path: Path) -> list[Diagnostic]:
        try:
            src = path.read_text(encoding="utf-8", errors="replace")
        except Exception as e:  # unreadable → nothing to report, fail-open
            logger.debug("PyCompileChecker read failed (fail-open): {}", e)
            return []
        try:
            compile(src, str(path), "exec")
            return []
        except SyntaxError as e:
            return [Diagnostic(
                severity="error",
                line=e.lineno or 1,
                col=e.offset or 1,
                code="E999",
                message=f"SyntaxError: {e.msg}",
            )]
        except ValueError as e:
            # e.g. source containing null bytes — surface as a syntax-level error
            return [Diagnostic(severity="error", line=1, col=1, code="E999",
                               message=f"SyntaxError: {e}")]
