from echo_agent.config.schema import Config, ValidationConfig


def test_validation_config_defaults():
    c = ValidationConfig()
    assert c.enabled is True
    assert c.timeout_sec == 5.0
    assert c.max_diagnostics == 10
    assert c.max_file_size_kb == 512


def test_config_has_validation_field():
    cfg = Config()
    assert isinstance(cfg.validation, ValidationConfig)
    assert cfg.validation.enabled is True
