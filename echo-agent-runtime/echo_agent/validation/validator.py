# echo_agent/validation/validator.py
"""Validator: run matching checkers on one file, then dedup/filter/truncate."""
from __future__ import annotations

import asyncio
from pathlib import Path

from loguru import logger

from echo_agent.validation.checkers import Checker, Diagnostic


class Validator:
    def __init__(
        self,
        checkers: list[Checker] | None = None,
        *,
        timeout_sec: float = 5.0,
        max_diagnostics: int = 10,
        max_file_size_kb: int = 512,
    ) -> None:
        self._checkers = checkers if checkers is not None else []
        self._timeout_sec = timeout_sec
        self._max_diagnostics = max_diagnostics
        self._max_file_size_kb = max_file_size_kb

    async def validate(self, path: Path) -> list[Diagnostic]:
        try:
            matching = [c for c in self._checkers if c.can_check(path)]
            if not matching:
                return []
            try:
                if path.stat().st_size > self._max_file_size_kb * 1024:
                    return []
            except OSError:
                return []
            diags = await asyncio.wait_for(
                self._run_all(matching, path), timeout=self._timeout_sec,
            )
        except asyncio.TimeoutError:
            logger.debug("validation timed out after {}s (fail-open): {}", self._timeout_sec, path)
            return []
        except Exception as e:  # never block the write
            logger.debug("validation failed (fail-open): {}", e)
            return []
        errors = [d for d in diags if d.severity == "error"]
        deduped = self._dedup(errors)
        deduped.sort(key=lambda d: (d.line, d.col))
        return deduped  # full list; truncation happens in format_diagnostics

    async def _run_all(self, checkers: list[Checker], path: Path) -> list[Diagnostic]:
        out: list[Diagnostic] = []
        for c in checkers:
            out.extend(await c.check(path))
        return out

    @staticmethod
    def _dedup(diags: list[Diagnostic]) -> list[Diagnostic]:
        # Collapse by position only: compile and ruff report the same syntax
        # error at the same (line, col) but with different message text, so a
        # message-sensitive key would never collapse them. Last writer wins, so
        # the more specific checker later in the registry (ruff) overrides the
        # syntax floor (PyCompile) at a shared position.
        seen: dict[tuple[int, int], Diagnostic] = {}
        for d in diags:
            seen[(d.line, d.col)] = d
        return list(seen.values())

    def format_diagnostics(self, diags: list[Diagnostic], filename: str) -> str:
        total = len(diags)
        shown = diags[: self._max_diagnostics]
        lines = [f"⚠ 写后校验发现 {total} 个错误 ({filename})："]
        for d in shown:
            lines.append(f"  L{d.line}:{d.col} {d.message} [{d.code}]")
        if total > len(shown):
            lines.append(f"  … 还有 {total - len(shown)} 个错误未列出")
        return "\n".join(lines)
