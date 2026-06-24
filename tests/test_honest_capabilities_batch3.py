from echo_agent.config.schema import MemoryConfig


def test_auto_resolve_contradictions_defaults_false():
    cfg = MemoryConfig()
    assert cfg.auto_resolve_contradictions is False
