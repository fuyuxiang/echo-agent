# tests/test_session_filename_encoding.py
import json
from pathlib import Path

import pytest

from echo_agent.session.manager import Session, SessionManager


def test_colon_and_underscore_keys_do_not_collide(tmp_path: Path):
    mgr = SessionManager(sessions_dir=tmp_path / "sessions")
    p_colon = mgr._session_path("a:b")
    p_under = mgr._session_path("a_b")
    p_slash = mgr._session_path("a/b")
    assert p_colon != p_under
    assert p_slash != p_under
    assert p_colon != p_slash


@pytest.mark.asyncio
async def test_roundtrip_with_encoded_name(tmp_path: Path):
    mgr = SessionManager(sessions_dir=tmp_path / "sessions")
    session = Session(key="chan:room/42")
    session.add_message("user", "hi")
    await mgr.save(session)
    await mgr.invalidate("chan:room/42")
    loaded = await mgr.get_or_create("chan:room/42")
    assert loaded.messages[0]["content"] == "hi"


def test_migrates_legacy_underscore_filename(tmp_path: Path):
    sessions = tmp_path / "sessions"
    sessions.mkdir(parents=True)
    # Simulate an old file written under the lossy scheme for key "tg:99".
    legacy = sessions / "tg_99.jsonl"
    legacy.write_text(
        json.dumps({"_type": "metadata", "key": "tg:99", "status": "active"}) + "\n",
        encoding="utf-8",
    )
    # Constructing the manager triggers the one-time migration.
    mgr = SessionManager(sessions_dir=sessions)
    new_path = mgr._session_path("tg:99")
    assert new_path.exists()
    assert not legacy.exists()

    # Idempotent: a second construction must not error or double-move.
    mgr2 = SessionManager(sessions_dir=sessions)
    assert mgr2._session_path("tg:99").exists()
