"""Tests for multi-agent error classification, user messages, and audit logging."""

import json
from pathlib import Path

import pytest

from echo_agent.agent.multi_agent.error_types import ToolErrorType, classify_tool_error
from echo_agent.agent.multi_agent.error_messages import (
    get_user_friendly_message,
    sanitize_error_for_user,
)
from echo_agent.agent.multi_agent.audit import DispatchAuditLog


# ── classify_tool_error ─────────────────────────────────────────────────────


class TestClassifyToolError:
    @pytest.mark.parametrize("text", ["not configured", "missing api key", "API_KEY not set"])
    def test_config_patterns(self, text: str):
        assert classify_tool_error(text) == ToolErrorType.CONFIG

    @pytest.mark.parametrize("text", ["unauthorized access", "HTTP 403 Forbidden"])
    def test_auth_patterns(self, text: str):
        assert classify_tool_error(text) == ToolErrorType.AUTH

    @pytest.mark.parametrize("text", ["429 Too Many Requests", "too many requests please wait"])
    def test_rate_limit_patterns(self, text: str):
        assert classify_tool_error(text) == ToolErrorType.RATE_LIMIT

    @pytest.mark.parametrize("text", ["request timeout", "502 Bad Gateway"])
    def test_transient_patterns(self, text: str):
        assert classify_tool_error(text) == ToolErrorType.TRANSIENT

    def test_unknown_when_no_match(self):
        assert classify_tool_error("something completely unrelated") == ToolErrorType.UNKNOWN


# ── get_user_friendly_message ───────────────────────────────────────────────


class TestGetUserFriendlyMessage:
    def test_with_enum_value(self):
        msg = get_user_friendly_message(ToolErrorType.CONFIG)
        assert "暂未配置" in msg

    def test_with_valid_string(self):
        msg = get_user_friendly_message("rate_limit")
        assert "受限" in msg

    def test_with_invalid_string_returns_fallback(self):
        msg = get_user_friendly_message("non_existent_type")
        assert "遇到了问题" in msg


# ── sanitize_error_for_user ─────────────────────────────────────────────────


class TestSanitizeErrorForUser:
    def test_usable_output_returned(self):
        result = sanitize_error_for_user("some error", "Here is a useful partial result")
        assert result == "Here is a useful partial result"

    def test_error_prefix_returns_generic(self):
        result = sanitize_error_for_user("err", "Error: stack overflow at line 42")
        assert "暂时无法完成" in result

    def test_empty_output_returns_generic(self):
        result = sanitize_error_for_user("some error", "")
        assert "暂时无法完成" in result


# ── DispatchAuditLog ────────────────────────────────────────────────────────


class TestDispatchAuditLog:
    def test_write_creates_parent_and_writes_json_line(self, tmp_path: Path):
        log_path = tmp_path / "sub" / "dir" / "audit.jsonl"
        audit = DispatchAuditLog(log_path)
        audit.write({"action": "dispatch", "worker": "coder"})

        assert log_path.exists()
        line = json.loads(log_path.read_text().strip())
        assert "ts" in line
        assert line["action"] == "dispatch"
        assert line["worker"] == "coder"

    def test_write_appends_multiple_lines(self, tmp_path: Path):
        log_path = tmp_path / "audit.jsonl"
        audit = DispatchAuditLog(log_path)
        audit.write({"seq": 1})
        audit.write({"seq": 2})

        lines = [json.loads(line) for line in log_path.read_text().strip().splitlines()]
        assert len(lines) == 2
        assert lines[0]["seq"] == 1
        assert lines[1]["seq"] == 2
