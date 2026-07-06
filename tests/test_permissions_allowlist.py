"""Tests for ApprovalAllowlist — pattern keys, approval levels, persistence."""

from __future__ import annotations

import json


from echo_agent.permissions.allowlist import (
    ApprovalAllowlist,
    ApprovalLevel,
    build_pattern_key,
)


# ══════════════════════════════════════════════════════════════════════════════
# build_pattern_key
# ══════════════════════════════════════════════════════════════════════════════


class TestBuildPatternKey:
    def test_exec_tool(self):
        key = build_pattern_key("exec", {"command": "ls -la /tmp"})
        assert key == "exec:ls"

    def test_exec_tool_with_path(self):
        key = build_pattern_key("exec", {"command": "/usr/bin/git status"})
        assert key == "exec:git"

    def test_exec_tool_empty_command(self):
        key = build_pattern_key("exec", {"command": ""})
        assert key == "exec:unknown"

    def test_execute_code_tool(self):
        key = build_pattern_key("execute_code", {"language": "python"})
        assert key == "code:python"

    def test_execute_code_no_language(self):
        key = build_pattern_key("execute_code", {})
        assert key == "code:unknown"

    def test_process_tool(self):
        key = build_pattern_key("process", {"command": "node server.js"})
        assert key == "process:node"

    def test_generic_tool(self):
        key = build_pattern_key("web_search", {"query": "hello"})
        assert key == "tool:web_search"

    def test_generic_tool_no_args(self):
        key = build_pattern_key("memory", {})
        assert key == "tool:memory"


# ══════════════════════════════════════════════════════════════════════════════
# is_approved
# ══════════════════════════════════════════════════════════════════════════════


class TestIsApproved:
    def test_not_approved_by_default(self):
        al = ApprovalAllowlist()
        assert al.is_approved("session_1", "exec:ls") is False

    def test_session_approval(self):
        al = ApprovalAllowlist()
        al.approve("session_1", "exec:ls", ApprovalLevel.SESSION)
        assert al.is_approved("session_1", "exec:ls") is True
        # Different session should not see it
        assert al.is_approved("session_2", "exec:ls") is False

    def test_permanent_approval(self):
        al = ApprovalAllowlist()
        al.approve("session_1", "exec:git", ApprovalLevel.ALWAYS)
        # Permanent is visible from any session
        assert al.is_approved("session_1", "exec:git") is True
        assert al.is_approved("session_2", "exec:git") is True


# ══════════════════════════════════════════════════════════════════════════════
# approve levels
# ══════════════════════════════════════════════════════════════════════════════


class TestApprove:
    def test_once_does_not_persist(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:ls", ApprovalLevel.ONCE)
        # ONCE approval is not stored
        assert al.is_approved("s1", "exec:ls") is False

    def test_session_approval(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:ls", ApprovalLevel.SESSION)
        assert al.is_approved("s1", "exec:ls") is True

    def test_always_approval(self):
        al = ApprovalAllowlist()
        al.approve("s1", "tool:memory", ApprovalLevel.ALWAYS)
        assert al.is_approved("s1", "tool:memory") is True
        assert al.is_approved("any_session", "tool:memory") is True


# ══════════════════════════════════════════════════════════════════════════════
# clear_session
# ══════════════════════════════════════════════════════════════════════════════


class TestClearSession:
    def test_clear_removes_session_approvals(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:ls", ApprovalLevel.SESSION)
        al.approve("s1", "exec:git", ApprovalLevel.SESSION)
        assert al.is_approved("s1", "exec:ls") is True

        al.clear_session("s1")
        assert al.is_approved("s1", "exec:ls") is False
        assert al.is_approved("s1", "exec:git") is False

    def test_clear_does_not_affect_permanent(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:git", ApprovalLevel.ALWAYS)
        al.clear_session("s1")
        # Permanent approvals remain
        assert al.is_approved("s1", "exec:git") is True

    def test_clear_nonexistent_session(self):
        al = ApprovalAllowlist()
        # Should not raise
        al.clear_session("nonexistent")


# ══════════════════════════════════════════════════════════════════════════════
# Persistence (write/read file)
# ══════════════════════════════════════════════════════════════════════════════


class TestPersistence:
    def test_save_and_load(self, tmp_path):
        store_file = tmp_path / "allowlist.json"
        al = ApprovalAllowlist(store_path=store_file)
        al.approve("s1", "exec:git", ApprovalLevel.ALWAYS)
        al.approve("s1", "tool:memory", ApprovalLevel.ALWAYS)

        # Verify file was written
        assert store_file.exists()
        data = json.loads(store_file.read_text())
        assert "permanent" in data
        assert "exec:git" in data["permanent"]
        assert "tool:memory" in data["permanent"]

        # Load into a new instance
        al2 = ApprovalAllowlist(store_path=store_file)
        assert al2.is_approved("any", "exec:git") is True
        assert al2.is_approved("any", "tool:memory") is True

    def test_load_missing_file(self, tmp_path):
        store_file = tmp_path / "nonexistent.json"
        # Should not raise
        al = ApprovalAllowlist(store_path=store_file)
        assert al.is_approved("s1", "anything") is False

    def test_session_approvals_not_persisted(self, tmp_path):
        store_file = tmp_path / "allowlist.json"
        al = ApprovalAllowlist(store_path=store_file)
        al.approve("s1", "exec:ls", ApprovalLevel.SESSION)

        # Session approvals don't trigger save
        if store_file.exists():
            data = json.loads(store_file.read_text())
            assert "exec:ls" not in data.get("permanent", [])


# ══════════════════════════════════════════════════════════════════════════════
# Family-wildcard (SESSION_ALL) — "approve all exec for this session"
# ══════════════════════════════════════════════════════════════════════════════


class TestFamilyWildcard:
    def test_wildcard_matches_any_command_in_family(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:*", ApprovalLevel.SESSION_ALL)
        # The prompted command AND later, differently-named ones all pass.
        assert al.is_approved("s1", "exec:pip") is True
        assert al.is_approved("s1", "exec:ffprobe") is True
        assert al.is_approved("s1", "exec:find") is True

    def test_wildcard_does_not_cross_families(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:*", ApprovalLevel.SESSION_ALL)
        # code: and tool: families are untouched by an exec wildcard.
        assert al.is_approved("s1", "code:python") is False
        assert al.is_approved("s1", "tool:cronjob") is False

    def test_wildcard_is_session_scoped(self):
        al = ApprovalAllowlist()
        al.approve("s1", "exec:*", ApprovalLevel.SESSION_ALL)
        # A different session does not inherit the grant.
        assert al.is_approved("s2", "exec:pip") is False

    def test_wildcard_not_written_to_disk(self, tmp_path):
        store_file = tmp_path / "allowlist.json"
        al = ApprovalAllowlist(store_path=store_file)
        al.approve("s1", "exec:*", ApprovalLevel.SESSION_ALL)
        # SESSION_ALL is in-memory only; nothing lands in the permanent store.
        if store_file.exists():
            data = json.loads(store_file.read_text())
            assert "exec:*" not in data.get("permanent", [])
        al2 = ApprovalAllowlist(store_path=store_file)
        assert al2.is_approved("s1", "exec:pip") is False
