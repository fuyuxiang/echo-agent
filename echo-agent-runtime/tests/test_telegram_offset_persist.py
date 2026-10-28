"""Tests for Telegram long-poll offset persistence.

Guards the P1 fix: a restart must not re-pull (and re-answer) updates the
previous process already acknowledged. The offset is keyed by bot_id (the
filename), so a different bot reads a different file and a token rotation on the
same bot keeps its cursor — there is no token check to get wrong.
"""

from pathlib import Path

from echo_agent.channels.telegram import (
    _load_offset,
    _offset_path,
    _save_offset,
)

BOT = "123456"


class TestOffsetPersistence:
    def test_load_returns_zero_when_no_file(self, tmp_path: Path):
        assert _load_offset(tmp_path, BOT) == 0

    def test_save_then_load_roundtrip(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, 42)
        assert _load_offset(tmp_path, BOT) == 42

    def test_offset_file_lives_under_data_dir_keyed_by_bot(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, 7)
        assert _offset_path(tmp_path, BOT) == tmp_path / f"{BOT}.offset.json"
        assert _offset_path(tmp_path, BOT).exists()

    def test_different_bot_uses_different_file(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, 99)
        # A different bot behind the same slot has a different bot_id, so it
        # reads its own (absent) file and starts from 0 without touching BOT's.
        assert _load_offset(tmp_path, "999999") == 0
        assert _load_offset(tmp_path, BOT) == 99

    def test_corrupt_file_degrades_to_zero(self, tmp_path: Path):
        path = _offset_path(tmp_path, BOT)
        path.write_text("not json{", encoding="utf-8")
        assert _load_offset(tmp_path, BOT) == 0

    def test_structurally_wrong_json_degrades_to_zero(self, tmp_path: Path):
        # Regression: valid JSON that is not an object (e.g. a list) must not
        # raise AttributeError/TypeError — it should degrade to 0 like any other
        # unreadable state, otherwise the channel fails to start.
        for payload in ("[]", "null", "42", '{"offset": "abc"}', "{}"):
            path = _offset_path(tmp_path, BOT)
            path.write_text(payload, encoding="utf-8")
            assert _load_offset(tmp_path, BOT) == 0, payload

    def test_save_to_missing_dir_does_not_raise(self, tmp_path: Path):
        missing = tmp_path / "does" / "not" / "exist"
        # Best-effort: a bad path logs a warning but must never crash the poll loop.
        _save_offset(missing, BOT, 5)

    def test_save_is_atomic_leaves_no_temp_file(self, tmp_path: Path):
        _save_offset(tmp_path, BOT, 11)
        # The temp file used for the atomic os.replace must not linger.
        assert not (tmp_path / f"{BOT}.offset.json.tmp").exists()
