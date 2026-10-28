from echo_agent.config.schema import Config, HeartbeatConfig


def test_heartbeat_defaults_present():
    cfg = Config()
    hb = cfg.agent.heartbeat
    assert isinstance(hb, HeartbeatConfig)
    assert hb.enabled is True
    assert hb.first_delay_sec == 30
    assert hb.min_interval_sec == 60
    assert hb.verbosity == "key_milestones"
    assert "{elapsed}" in hb.template and "{activity}" in hb.template


def test_heartbeat_config_new_fields_defaults():
    cfg = HeartbeatConfig()
    assert cfg.enabled is True
    assert cfg.first_delay_sec == 30
    assert cfg.min_interval_sec == 60
    assert cfg.verbosity == "key_milestones"
    assert "{activity}" in cfg.template and "{elapsed}" in cfg.template


def test_heartbeat_config_dropped_fields():
    cfg = HeartbeatConfig()
    assert not hasattr(cfg, "on_uneditable")
    assert not hasattr(cfg, "interval_sec")


def test_verbosity_rejects_unknown_value():
    import pytest
    from pydantic import ValidationError
    with pytest.raises(ValidationError):
        HeartbeatConfig(verbosity="loud")
