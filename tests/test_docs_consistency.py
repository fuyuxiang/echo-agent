"""CI guard: committed reference docs must match current schema metadata."""
from __future__ import annotations

from pathlib import Path

from echo_agent.config.docgen import render_backlog, render_markdown, render_yaml

_DOCS = Path(__file__).resolve().parent.parent / "docs"


def _check(name: str, rendered: str):
    path = _DOCS / name
    assert path.exists(), f"缺少生成产物 {name},请运行 `python -m echo_agent config gen-docs`"
    assert path.read_text(encoding="utf-8") == rendered, (
        f"{name} 与当前 schema 不一致,请重新运行 `python -m echo_agent config gen-docs` 并提交"
    )


def test_yaml_zh_consistent():
    _check("config-reference.yaml", render_yaml("zh"))


def test_yaml_en_consistent():
    _check("config-reference.en.yaml", render_yaml("en"))


def test_md_zh_consistent():
    _check("config-reference.md", render_markdown("zh"))


def test_md_en_consistent():
    _check("config-reference.en.md", render_markdown("en"))


def test_backlog_consistent():
    _check("config-dead-fields-backlog.md", render_backlog())
