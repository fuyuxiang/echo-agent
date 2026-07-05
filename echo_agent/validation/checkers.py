"""Checkers turn a written file into a list of Diagnostics.

First release ships Python only: PyCompileChecker (in-process syntax floor,
zero deps) + RuffChecker (semantic diagnostics when ruff is on PATH).
"""
from __future__ import annotations

import asyncio
import json
import shutil
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


async def _reap(proc: asyncio.subprocess.Process) -> None:
    """Terminate and collect a subprocess so a cancelled check leaves no orphan."""
    try:
        proc.kill()
    except ProcessLookupError:
        return  # already exited — nothing to reap
    try:
        await proc.wait()
    except ProcessLookupError:
        pass


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


class RuffChecker:
    """Semantic diagnostics for .py files via the ruff CLI, when available.

    Availability is probed once at construction. If ruff is not on PATH,
    ``can_check`` returns False and the checker is a silent no-op — the
    PyCompileChecker floor still runs.
    """

    def __init__(self) -> None:
        self._ruff = shutil.which("ruff")

    def can_check(self, path: Path) -> bool:
        return self._ruff is not None and path.suffix == ".py"

    async def check(self, path: Path) -> list[Diagnostic]:
        if self._ruff is None:
            return []
        try:
            proc = await asyncio.create_subprocess_exec(
                self._ruff, "check", "--output-format=json", "--force-exclude", str(path),
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            )
        except Exception as e:  # ruff vanished / spawn failed → fail-open
            logger.debug("RuffChecker spawn failed (fail-open): {}", e)
            return []
        # Spawn succeeded: any early exit (esp. CancelledError from an outer
        # wait_for timeout) must reap the child so it can't become an orphan.
        try:
            out, _err = await proc.communicate()
        except Exception as e:  # communicate failed → fail-open, but still reap
            logger.debug("RuffChecker communicate failed (fail-open): {}", e)
            await _reap(proc)
            return []
        except BaseException:  # includes CancelledError → reap, then propagate
            await _reap(proc)
            raise
        text = out.decode("utf-8", "replace").strip()
        if not text:
            return []
        try:
            items = json.loads(text)
        except json.JSONDecodeError as e:
            logger.debug("RuffChecker json parse failed (fail-open): {}", e)
            return []
        if not isinstance(items, list):
            # ruff normally emits a JSON array; anything else is unexpected shape
            logger.debug("RuffChecker got non-list JSON (fail-open): {}", type(items).__name__)
            return []
        diags: list[Diagnostic] = []
        for it in items:
            if not isinstance(it, dict):
                continue  # skip malformed entry, keep the rest
            loc = it.get("location")
            if not isinstance(loc, dict):
                loc = {}
            diags.append(Diagnostic(
                severity="error",  # ruff findings are all reported at error weight in v1
                line=int(loc.get("row", 1) or 1),
                col=int(loc.get("column", 1) or 1),
                code=str(it.get("code") or "RUFF"),
                message=str(it.get("message") or ""),
            ))
        return diags


def default_checkers() -> list[Checker]:
    """First-release registry: Python syntax floor + optional ruff semantic layer."""
    return [PyCompileChecker(), RuffChecker()]
