"""Tests for config reference doc generation."""
from __future__ import annotations

import yaml

from echo_agent.config.docgen import render_markdown, render_yaml


def test_yaml_includes_effective_excludes_dead():
    out = render_yaml("zh")
    # effective 字段出现(camelCase)
    assert "triggerRatio" in out
    # dead 字段不出现
    assert "archivalThreshold" not in out
    assert "reasoningEffort" not in out


def test_yaml_has_comment_with_default():
    out = render_yaml("zh")
    # 注释行包含中文说明与默认值
    assert "# " in out
    assert "0.7" in out  # compression.triggerRatio 默认值


def test_markdown_has_group_headers_and_choices():
    out = render_markdown("zh")
    assert "## security" in out or "## Security" in out
    # security.profile 是 effective Literal,应列出可选值
    assert "personal_cli" in out
    # dead 字段不出现
    assert "showToolCalls" not in out


def test_lang_switch_changes_desc():
    zh = render_yaml("zh")
    en = render_yaml("en")
    # 同一字段在两种语言下注释不同(desc_zh vs desc_en)
    assert zh != en


def test_yaml_is_parseable():
    # 渲染出的 YAML 必须合法可解析(跳过容器元素字段保证缩进合法)
    parsed = yaml.safe_load(render_yaml("zh"))
    assert isinstance(parsed, dict)


def test_yaml_roundtrip_preserves_none_and_bool():
    # 往返保真:None 默认值必须解析回 None(而非字符串 "None"),
    # 布尔默认值必须解析回真正的 bool(而非字符串 "True"/"False")。
    parsed = yaml.safe_load(render_yaml("zh"))
    assert parsed["channels"]["telegram"]["proxy"] is None
    assert parsed["channels"]["telegram"]["enabled"] is False


def test_markdown_includes_container_element_field():
    out = render_markdown("zh")
    # Markdown 渲染全部三类字段,含容器元素字段路径
    assert "providers[].apiKey" in out
