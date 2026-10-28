"""MemoryConfig 新字段与默认值变更测试。"""
from echo_agent.config.schema import MemoryConfig


def test_local_embedding_model_default():
    cfg = MemoryConfig()
    assert cfg.local_embedding_model == "BAAI/bge-small-zh-v1.5"


def test_local_embedding_model_empty_disables():
    cfg = MemoryConfig(local_embedding_model="")
    assert cfg.local_embedding_model == ""


def test_vector_dimensions_default_auto():
    cfg = MemoryConfig()
    assert cfg.vector_dimensions == 0


def test_vector_dimensions_explicit_respected():
    cfg = MemoryConfig(vector_dimensions=1536)
    assert cfg.vector_dimensions == 1536
