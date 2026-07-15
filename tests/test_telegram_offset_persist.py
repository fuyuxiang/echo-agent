"""Tests for Telegram long-poll offset persistence.

Guards the P1 fix: a restart must not re-pull (and re-answer) updates the
previous process already acknowledged. Offset is keyed by bot_id and bound to a
token hash so a swapped bot token resets the cursor instead of skipping backlog.
"""

from pathlib import Path

from echo_agent.channels.telegram import (
    _load_offset,
    _offset_path,
    _save_offset,
    _token_hash,
)

BOT = "123456"
TOKEN = "123456:AAExampleToken"
THASH = _token_hash(TOKEN)


class TestOffsetPersistence:
    def test_load_returns_zero_when_no_file(self, tmp_path: Path):
        assert _load_offset(tmp_path, BOT, THASH) == 0

    def test_save_then_load_roundtrip(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, THASH, 42)
        assert _load_offset(tmp_path, BOT, THASH) == 42

    def test_offset_file_lives_under_data_dir_keyed_by_bot(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, THASH, 7)
        assert _offset_path(tmp_path, BOT) == tmp_path / f"{BOT}.offset.json"
        assert _offset_path(tmp_path, BOT).exists()

    def test_token_change_resets_offset(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, THASH, 99)
        # A different token behind the same bot slot must not reuse the old
        # cursor (that would silently skip the new bot's backlog).
        other = _token_hash("999999:DifferentToken")
        assert _load_offset(tmp_path, BOT, other) == 0

    def test_corrupt_file_degrades_to_zero(self, tmp_path: Path):
        path = _offset_path(tmp_path, BOT)
        path.write_text("not json{", encoding="utf-8")
        assert _load_offset(tmp_path, BOT, THASH) == 0

    def test_save_to_missing_dir_does_not_raise(self, tmp_path: Path):
        missing = tmp_path / "does" / "not" / "exist"
        # Best-effort: a bad path logs a warning but must never crash the poll loop.
        _save_offset(missing, BOT, THASH, 5)

    def test_token_hash_stable_and_empty_safe(self):
        assert _token_hash(TOKEN) == _token_hash(TOKEN)
        assert _token_hash("") == ""
