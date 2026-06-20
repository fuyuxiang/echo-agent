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


def test_container_field_descends_into_submodel():
    paths = {f.path for f in iter_fields(Config)}
    assert "tools.mcpServers" in paths
    assert "tools.mcpServers{}.command" in paths
    assert "models.providers[].apiKey" in paths
    assert "models.routes[].model" in paths
    assert "multiAgent.workerProfiles[].instructions" in paths
    assert "gateway.platforms{}.rateLimitRpm" in paths


def test_extra_defaults_to_empty_dict():
    fields = {f.path: f for f in iter_fields(Config)}
    # 补元数据后 workspace 仍应有 status 字段
    assert isinstance(fields["workspace"].extra, dict)


def test_dead_fields_are_marked():
    fields = {f.snake_path: f for f in iter_fields(Config)}
    # 抽查几个已知死字段
    assert fields["storage.backend"].extra.get("status") == "dead"
    assert fields["agent.reasoning_effort"].extra.get("status") == "dead"
    assert fields["memory.archival_threshold"].extra.get("disposition") == "fix"
    assert fields["models.cost_limit_daily_usd"].extra.get("disposition") == "remove"


def test_effective_fields_have_desc():
    fields = {f.snake_path: f for f in iter_fields(Config)}
    info = fields["compression.trigger_ratio"]
    assert info.extra.get("status") == "effective"
    assert info.extra.get("desc_zh")
    assert info.extra.get("desc_en")
    assert info.extra.get("ref")


def test_all_fields_have_status():
    bad = [
        f.path
        for f in iter_fields(Config)
        if f.extra.get("status") not in ("effective", "dead")
    ]
    assert bad == [], f"字段缺 status: {bad}"
