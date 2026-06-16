from pathlib import Path

from echo_agent.config.loader import profile_explicitly_set


def _write_yaml(tmp_path: Path, body: str) -> Path:
    f = tmp_path / "echo-agent.yaml"
    f.write_text(body, encoding="utf-8")
    return f


def test_yaml_with_profile_is_explicit(tmp_path):
    cfg = _write_yaml(tmp_path, "security:\n  profile: personal_cli\n")
    assert profile_explicitly_set(cfg) is True


def test_yaml_without_profile_is_not_explicit(tmp_path):
    cfg = _write_yaml(tmp_path, "security:\n  admin_users: []\n")
    assert profile_explicitly_set(cfg) is False


def test_no_yaml_is_not_explicit(tmp_path):
    assert profile_explicitly_set(None) is False


def test_env_profile_is_explicit(tmp_path, monkeypatch):
    cfg = _write_yaml(tmp_path, "security:\n  admin_users: []\n")
    monkeypatch.setenv("ECHO_AGENT_SECURITY__PROFILE", "public_gateway")
    assert profile_explicitly_set(cfg) is True


from echo_agent.app import _apply_gateway_profile_default


def test_applies_public_gateway_when_not_explicit(tmp_path):
    from echo_agent.config.schema import Config

    cfg = Config()  # 默认 security.profile == "personal_cli"
    assert cfg.security.profile == "personal_cli"
    _apply_gateway_profile_default(cfg, config_path=None)
    assert cfg.security.profile == "public_gateway"


def test_respects_explicit_profile(tmp_path):
    from echo_agent.config.schema import Config

    cfg = _write_yaml(tmp_path, "security:\n  profile: personal_cli\n")
    config = Config(security={"profile": "personal_cli"})
    _apply_gateway_profile_default(config, config_path=cfg)
    assert config.security.profile == "personal_cli"  # 显式配置不覆盖
