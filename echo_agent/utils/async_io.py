"""Async file I/O utilities — wraps blocking operations via asyncio.to_thread."""

from __future__ import annotations

import asyncio
import json
import os
import tempfile
from pathlib import Path
from typing import Any


async def atomic_write_json(path: Path, data: Any, *, indent: int = 2) -> None:
    """Atomically write JSON data to *path* without blocking the event loop."""

    def _sync() -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(data, f, ensure_ascii=False, indent=indent)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, str(path))
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    await asyncio.to_thread(_sync)


async def atomic_write_lines(path: Path, lines: list[str]) -> None:
    """Atomically write lines (JSONL style) to *path* without blocking."""

    def _sync() -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                for line in lines:
                    f.write(line + "\n")
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, str(path))
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    await asyncio.to_thread(_sync)


async def read_json(path: Path) -> Any:
    """Read and parse a JSON file without blocking."""

    def _sync() -> Any:
        with open(path, encoding="utf-8") as f:
            return json.load(f)

    return await asyncio.to_thread(_sync)


async def read_lines(path: Path) -> list[str]:
    """Read all lines from a file without blocking."""

    def _sync() -> list[str]:
        with open(path, encoding="utf-8") as f:
            return f.readlines()

    return await asyncio.to_thread(_sync)


async def file_exists(path: Path) -> bool:
    """Check file existence without blocking."""
    return await asyncio.to_thread(path.exists)
