"""Session file scans must not block the event loop.

``list_sessions_async`` and ``cleanup_expired`` in file mode walk the sessions
directory and read one line from every ``.jsonl``. With a large session
inventory that is disk I/O on the gateway's loop thread — the same class of
stall openclaw fixed with "keep Gateway responsive during transcript archive
scans". The scan itself is extracted into a synchronous helper dispatched via
``asyncio.to_thread``; the async side only awaits the per-session transitions.
"""

from __future__ import annotations

import json
import threading
from pathlib import Path

import pytest

from echo_agent.session.manager import Session, SessionManager


def _write_meta(sessions: Path, filename: str, key: str, updated_at: str) -> None:
    sessions.mkdir(parents=True, exist_ok=True)
    (sessions / filename).write_text(
        json.dumps({
            "_type": "metadata",
            "key": key,
            "status": "active",
            "updated_at": updated_at,
        }) + "\n",
        encoding="utf-8",
    )


@pytest.mark.asyncio
async def test_list_sessions_async_reads_file_mode_correctly(tmp_path: Path):
    sessions = tmp_path / "sessions"
    _write_meta(sessions, "a.jsonl", "tg:1", "2026-01-01T00:00:00")
    _write_meta(sessions, "b.jsonl", "tg:2", "2026-01-02T00:00:00")

    mgr = SessionManager(sessions_dir=sessions, expiry_hours=1)
    listed = await mgr.list_sessions_async()
    keys = {item["key"] for item in listed}
    assert keys == {"tg:1", "tg:2"}


@pytest.mark.asyncio
async def test_file_scan_runs_off_the_event_loop(tmp_path: Path, monkeypatch):
    sessions = tmp_path / "sessions"
    _write_meta(sessions, "a.jsonl", "tg:1", "2026-01-01T00:00:00")

    mgr = SessionManager(sessions_dir=sessions, expiry_hours=1)
    loop_thread_id = threading.get_ident()
    scanned_in: list[int] = []

    original = mgr._scan_session_files_sync

    def _wrapped() -> list[dict]:
        scanned_in.append(threading.get_ident())
        return original()

    monkeypatch.setattr(mgr, "_scan_session_files_sync", _wrapped)

    await mgr.list_sessions_async()

    assert scanned_in, "file scan never ran"
    assert scanned_in[0] != loop_thread_id, (
        "file scan ran on the event-loop thread; it must run in a worker thread"
    )


@pytest.mark.asyncio
async def test_cleanup_scan_runs_off_loop_and_expires_file(tmp_path: Path, monkeypatch):
    sessions = tmp_path / "sessions"
    _write_meta(sessions, "cli%3Aone.jsonl", "cli:one", "2020-01-01T00:00:00")
    mgr = SessionManager(sessions_dir=sessions, expiry_hours=1)
    loop_thread_id = threading.get_ident()
    scan_threads: list[int] = []
    original = mgr._scan_session_files_sync

    def _wrapped():
        scan_threads.append(threading.get_ident())
        return original()

    monkeypatch.setattr(mgr, "_scan_session_files_sync", _wrapped)
    assert await mgr.cleanup_expired() == 1
    assert len(scan_threads) == 1
    assert scan_threads[0] != loop_thread_id
    first_line = (sessions / "cli%3Aone.jsonl").read_text(encoding="utf-8").splitlines()[0]
    assert json.loads(first_line)["status"] == "expired"


@pytest.mark.asyncio
async def test_legacy_storage_without_listing_uses_cache_without_recursion(tmp_path: Path):
    mgr = SessionManager(sessions_dir=tmp_path / "sessions", storage=object())
    mgr._cache["cli:one"] = Session(key="cli:one")
    listed = await mgr.list_sessions_async()
    assert [item["key"] for item in listed] == ["cli:one"]


def test_scan_keeps_only_listing_fields(tmp_path: Path):
    sessions = tmp_path / "sessions"
    sessions.mkdir()
    (sessions / "one.jsonl").write_text(json.dumps({
        "_type": "metadata", "key": "cli:one", "status": "active",
        "updated_at": "2026-01-01T00:00:00", "metadata": {"large": "x" * 10000},
    }) + "\n", encoding="utf-8")
    mgr = SessionManager(sessions_dir=sessions)
    rows = mgr._scan_session_files_sync()
    assert len(rows) == 1
    assert "metadata" not in rows[0][0]
