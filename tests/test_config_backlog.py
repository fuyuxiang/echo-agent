"""Tests for dead-field backlog generation."""
from __future__ import annotations

from echo_agent.config.docgen import render_backlog
from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config


def test_backlog_reflects_no_dead_fields():
    # Task 7 收敛后 schema 无 dead 字段,backlog 不应再列出任何 fix/remove 条目。
    out = render_backlog()
    assert "## fix" not in out
    assert "## remove" not in out


def test_backlog_matches_dead_field_state():
    # backlog 内容必须与 schema 中的 dead 字段一致(当前应为空)。
    dead = [f.snake_path for f in iter_fields(Config) if f.extra.get("status") == "dead"]
    out = render_backlog()
    for path in dead:
        assert path in out


def test_backlog_excludes_effective_fields():
    out = render_backlog()
    assert "triggerRatio" not in out
