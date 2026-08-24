"""Tests for audit-log masking of secrets inside sequences.

``_mask_sensitive`` recursed into dicts but passed lists through verbatim, so
``["--token", "s3cr3t"]`` landed in the audit log in cleartext — and passing
credentials through ``args`` was exactly what the skill docs recommended, because
``skill_run``'s empty environment left no other way to supply them.
"""

from __future__ import annotations

from echo_agent.agent.tools.registry import _mask_sensitive


class TestDictMasking:
    def test_sensitive_key_masked(self):
        assert _mask_sensitive({"api_key": "sk-123"})["api_key"] == "***"

    def test_nested_dict_masked(self):
        out = _mask_sensitive({"outer": {"password": "hunter2"}})
        assert out["outer"]["password"] == "***"

    def test_ordinary_values_untouched(self):
        out = _mask_sensitive({"name": "demo", "count": 3})
        assert out == {"name": "demo", "count": 3}


class TestSequenceMasking:
    def test_argv_flag_value_pair_masked(self):
        """The documented-and-dangerous pattern."""
        out = _mask_sensitive({"args": ["--token", "s3cr3t", "--verbose"]})
        assert out["args"] == ["--token", "***", "--verbose"]
        assert "s3cr3t" not in str(out)

    def test_inline_equals_form_masked(self):
        out = _mask_sensitive({"args": ["--api-key=s3cr3t"]})
        assert out["args"] == ["--api-key=***"]

    def test_short_flag_masked(self):
        out = _mask_sensitive({"args": ["--password", "pw"]})
        assert out["args"] == ["--password", "***"]

    def test_multiple_secrets_in_one_argv(self):
        out = _mask_sensitive(
            {"args": ["--token", "a", "--secret", "b", "plain"]}
        )
        assert out["args"] == ["--token", "***", "--secret", "***", "plain"]

    def test_non_secret_flags_keep_their_values(self):
        """Over-masking would make audit logs useless for debugging."""
        out = _mask_sensitive({"args": ["--query", "hello world", "--limit", "10"]})
        assert out["args"] == ["--query", "hello world", "--limit", "10"]

    def test_nested_sequences_handled(self):
        out = _mask_sensitive({"args": [["--token", "x"], "plain"]})
        assert out["args"] == [["--token", "***"], "plain"]

    def test_dict_inside_list_masked(self):
        out = _mask_sensitive({"items": [{"token": "x"}, {"name": "y"}]})
        assert out["items"][0]["token"] == "***"
        assert out["items"][1]["name"] == "y"

    def test_tuple_becomes_masked_list(self):
        out = _mask_sensitive({"args": ("--token", "x")})
        assert out["args"] == ["--token", "***"]

    def test_non_string_items_survive(self):
        out = _mask_sensitive({"args": [1, None, True, "plain"]})
        assert out["args"] == [1, None, True, "plain"]

    def test_flag_at_end_without_value(self):
        out = _mask_sensitive({"args": ["--token"]})
        assert out["args"] == ["--token"]


class TestSkillRunAuditShape:
    def test_realistic_skill_run_params(self):
        params = {
            "name": "notion-sync",
            "script": "scripts/sync.py",
            "args": ["--token", "secret_abc123", "--database", "db1"],
        }
        out = _mask_sensitive(params)
        assert out["name"] == "notion-sync"
        assert out["script"] == "scripts/sync.py"
        assert "secret_abc123" not in str(out)
        assert "db1" in str(out)
