"""embedding_backend 配置字段的默认值与取值约束。"""
from echo_agent.config.schema import MemoryConfig
from echo_agent.config.metadata import iter_fields


def test_embedding_backend_defaults_to_auto():
    assert MemoryConfig().embedding_backend == "auto"


def test_embedding_backend_choices_exposed_via_literal():
    field = next(
        f for f in iter_fields(MemoryConfig) if f.snake_path.endswith("embedding_backend")
    )
    assert field.choices == ["auto", "local", "provider"]
