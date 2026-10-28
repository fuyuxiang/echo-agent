"""Tests for dead-field backlog generation."""
from __future__ import annotations

from echo_agent.config.docgen import render_backlog
from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config


def test_backlog_sections_track_dispositions():
    # 早先这里断言"schema 无 dead 字段",把一次收敛后的快照冻成了不变量:任何如实
    # 标注的新死字段都会让它失败,反而鼓励把死字段留标 effective。改为校验机制本身
    # —— 每个 disposition 有条目时才出现对应小节。
    out = render_backlog()
    dispositions = {
        f.extra.get("disposition", "keep")
        for f in iter_fields(Config)
        if f.extra.get("status") == "dead"
    }
    for disp in ("fix", "remove", "keep"):
        assert (f"## {disp}" in out) is (disp in dispositions)


def test_backlog_matches_dead_field_state():
    # backlog 必须列出 schema 中每个 dead 字段,一个不漏。
    dead = [f.snake_path for f in iter_fields(Config) if f.extra.get("status") == "dead"]
    out = render_backlog()
    for path in dead:
        assert path in out


def test_backlog_excludes_effective_fields():
    out = render_backlog()
    assert "triggerRatio" not in out
