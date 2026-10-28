"""Tests for multi-agent audit logging."""

import json
from pathlib import Path

from echo_agent.agent.multi_agent.audit import DispatchAuditLog


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
