"""Tests for async file I/O utilities."""

import json
from pathlib import Path

import pytest

from echo_agent.utils.async_io import (
    atomic_write_json,
    atomic_write_lines,
    file_exists,
    read_json,
    read_lines,
)


class TestAtomicWriteJson:
    @pytest.mark.asyncio
    async def test_writes_valid_json(self, tmp_path: Path):
        path = tmp_path / "test.json"
        data = {"key": "value", "num": 42, "nested": {"a": [1, 2, 3]}}
        await atomic_write_json(path, data)

        loaded = json.loads(path.read_text(encoding="utf-8"))
        assert loaded == data

    @pytest.mark.asyncio
    async def test_creates_parent_dirs(self, tmp_path: Path):
        path = tmp_path / "sub" / "dir" / "test.json"
        await atomic_write_json(path, {"ok": True})
        assert path.exists()

    @pytest.mark.asyncio
    async def test_atomic_no_partial_write(self, tmp_path: Path, monkeypatch):
        path = tmp_path / "atomic.json"
        await atomic_write_json(path, {"original": True})

        original_content = path.read_text(encoding="utf-8")

        def failing_replace(src, dst):
            import os
            os.unlink(src)
            raise OSError("disk full")

        monkeypatch.setattr("os.replace", failing_replace)

        with pytest.raises(OSError):
            await atomic_write_json(path, {"corrupted": True})

        assert path.read_text(encoding="utf-8") == original_content

    @pytest.mark.asyncio
    async def test_no_temp_files_left(self, tmp_path: Path):
        path = tmp_path / "clean.json"
        await atomic_write_json(path, [1, 2, 3])
        tmp_files = list(tmp_path.glob("*.tmp"))
        assert len(tmp_files) == 0


class TestAtomicWriteLines:
    @pytest.mark.asyncio
    async def test_writes_lines(self, tmp_path: Path):
        path = tmp_path / "lines.txt"
        await atomic_write_lines(path, ["line1", "line2", "line3"])
        content = path.read_text(encoding="utf-8")
        assert content == "line1\nline2\nline3\n"

    @pytest.mark.asyncio
    async def test_empty_lines(self, tmp_path: Path):
        path = tmp_path / "empty.txt"
        await atomic_write_lines(path, [])
        assert path.read_text(encoding="utf-8") == ""


class TestReadJson:
    @pytest.mark.asyncio
    async def test_reads_json(self, tmp_path: Path):
        path = tmp_path / "data.json"
        data = {"hello": "world"}
        path.write_text(json.dumps(data), encoding="utf-8")
        result = await read_json(path)
        assert result == data

    @pytest.mark.asyncio
    async def test_raises_on_missing_file(self, tmp_path: Path):
        path = tmp_path / "missing.json"
        with pytest.raises(FileNotFoundError):
            await read_json(path)


class TestReadLines:
    @pytest.mark.asyncio
    async def test_reads_lines(self, tmp_path: Path):
        path = tmp_path / "lines.txt"
        path.write_text("a\nb\nc\n", encoding="utf-8")
        result = await read_lines(path)
        assert result == ["a\n", "b\n", "c\n"]


class TestFileExists:
    @pytest.mark.asyncio
    async def test_existing_file(self, tmp_path: Path):
        path = tmp_path / "exists.txt"
        path.write_text("hi")
        assert await file_exists(path)

    @pytest.mark.asyncio
    async def test_missing_file(self, tmp_path: Path):
        path = tmp_path / "nope.txt"
        assert not await file_exists(path)
