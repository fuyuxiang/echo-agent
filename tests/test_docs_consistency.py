"""CI guard: schema 元数据驱动的文档渲染必须可正常执行。

注：config-reference 系列产物按项目规则不入库(已 gitignore),
故此处只校验渲染函数能产出非空内容,不校验落盘文件。
"""
from __future__ import annotations

from echo_agent.config.docgen import render_backlog, render_markdown, render_yaml


def _assert_renderable(rendered: str, label: str):
    assert isinstance(rendered, str), f"{label} 渲染结果应为 str"
    assert rendered.strip(), f"{label} 渲染结果不应为空"


def test_yaml_zh_renderable():
    _assert_renderable(render_yaml("zh"), "config-reference.yaml")


def test_yaml_en_renderable():
    _assert_renderable(render_yaml("en"), "config-reference.en.yaml")


def test_md_zh_renderable():
    _assert_renderable(render_markdown("zh"), "config-reference.md")


def test_md_en_renderable():
    _assert_renderable(render_markdown("en"), "config-reference.en.md")


def test_backlog_renderable():
    _assert_renderable(render_backlog(), "config-dead-fields-backlog.md")
