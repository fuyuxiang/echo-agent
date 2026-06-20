"""Tests for config field metadata traversal."""
from __future__ import annotations

from echo_agent.config.metadata import FieldInfo, iter_fields
from echo_agent.config.schema import Config


def test_iter_fields_returns_leaf_fields():
    fields = list(iter_fields(Config))
    assert all(isinstance(f, FieldInfo) for f in fields)
    paths = {f.path for f in fields}
    # 顶层标量
    assert "workspace" in paths
    # 嵌套叶子(camelCase 点路径)
    assert "memory.archivalThreshold" in paths
    assert "compression.triggerRatio" in paths
    # 不应把中间子模型本身当叶子产出
    assert "memory" not in paths
    assert "compression" not in paths


def test_field_info_has_snake_path_and_type():
    fields = {f.path: f for f in iter_fields(Config)}
    info = fields["memory.archivalThreshold"]
    assert info.snake_path == "memory.archival_threshold"
    assert "float" in info.type_str.lower()


def test_literal_choices_extracted():
    fields = {f.path: f for f in iter_fields(Config)}
    # SecurityConfig.profile 是 Literal["personal_cli","daemon","public_gateway"]
    info = fields["security.profile"]
    assert info.choices == ["personal_cli", "daemon", "public_gateway"]


def test_container_field_is_leaf_not_descended():
    paths = {f.path for f in iter_fields(Config)}
    # tools.mcpServers 是 dict[str, MCPServerConfig],按叶子处理
    assert "tools.mcpServers" in paths
    # 不应下钻进 MCPServerConfig 的字段
    assert not any(p.startswith("tools.mcpServers.") for p in paths)


def test_extra_defaults_to_empty_dict():
    fields = {f.path: f for f in iter_fields(Config)}
    # 尚未补元数据时 extra 为空 dict,不是 None
    assert fields["workspace"].extra == {}
