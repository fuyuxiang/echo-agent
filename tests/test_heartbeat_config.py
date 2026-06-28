from echo_agent.config.schema import Config, HeartbeatConfig


def test_heartbeat_defaults_present():
    cfg = Config()
    hb = cfg.agent.heartbeat
    assert isinstance(hb, HeartbeatConfig)
    assert hb.enabled is True
    assert hb.first_delay_sec == 30
    assert hb.interval_sec == 60
    assert hb.on_uneditable == "first_only"
    assert "{elapsed}" in hb.template and "{activity}" in hb.template


def test_heartbeat_on_uneditable_constrained():
    import pytest
    from pydantic import ValidationError
    with pytest.raises(ValidationError):
        HeartbeatConfig(on_uneditable="bogus")
