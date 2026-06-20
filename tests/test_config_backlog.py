"""Tests for dead-field backlog generation."""
from __future__ import annotations

from echo_agent.config.docgen import render_backlog


def test_backlog_groups_by_disposition():
    out = render_backlog()
    assert "fix" in out.lower()
    assert "remove" in out.lower()


def test_backlog_lists_known_dead_fields():
    out = render_backlog()
    assert "storage.backend" in out
    assert "reasoningEffort" in out or "reasoning_effort" in out
    # reason 文案出现
    assert "SQLiteBackend" in out


def test_backlog_excludes_effective_fields():
    out = render_backlog()
    assert "triggerRatio" not in out
