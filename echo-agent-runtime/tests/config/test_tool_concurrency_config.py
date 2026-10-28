from echo_agent.config.schema import Config


def test_tool_concurrency_defaults():
    cfg = Config()
    assert cfg.agent.tool_concurrency.enabled is True
    assert cfg.agent.tool_concurrency.max_concurrent == 4


def test_tool_concurrency_override():
    cfg = Config(agent={"tool_concurrency": {"max_concurrent": 1}})
    assert cfg.agent.tool_concurrency.max_concurrent == 1
    assert cfg.agent.tool_concurrency.enabled is True
