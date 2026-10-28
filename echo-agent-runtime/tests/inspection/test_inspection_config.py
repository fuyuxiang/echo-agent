from echo_agent.config.schema import AgentBehaviorConfig, Config, InspectionConfig


def test_inspection_config_defaults():
    c = InspectionConfig()
    assert c.enabled is False
    assert c.tick_interval_sec == 300
    assert c.inspect_file == "INSPECT.md"
    assert c.max_items_per_tick == 5
    assert c.deliver_channel == ""
    assert c.deliver_chat_id == ""


def test_inspection_mounted_on_agent_behavior():
    c = Config()
    assert isinstance(c.agent.inspection, InspectionConfig)
    assert isinstance(AgentBehaviorConfig().inspection, InspectionConfig)
