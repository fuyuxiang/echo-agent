from pathlib import Path

from echo_agent.app import _apply_gateway_profile_default, _gateway_profile_override
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


def test_public_gateway_denies_write_allows_read():
    from echo_agent.config.schema import Config
    from echo_agent.security.tool_policy import is_tool_allowed

    cfg = Config(security={"profile": "public_gateway"})
    assert is_tool_allowed(cfg, "write_file") is False
    assert is_tool_allowed(cfg, "exec") is False
    assert is_tool_allowed(cfg, "read_file") is True


def test_override_tightens_when_not_explicit():
    override = _gateway_profile_override(config_path=None)
    assert override == {"security": {"profile": "public_gateway"}}


def test_override_empty_when_explicit(tmp_path):
    cfg = _write_yaml(tmp_path, "security:\n  profile: personal_cli\n")
    assert _gateway_profile_override(config_path=str(cfg)) == {}


def test_override_still_tightens_with_full_tools_profile(tmp_path):
    # tools.profile: full but no explicit security.profile → still downgraded,
    # and the conflict warning path (full/coding branch) is exercised.
    cfg = _write_yaml(tmp_path, "tools:\n  profile: full\n")
    assert _gateway_profile_override(config_path=str(cfg)) == {
        "security": {"profile": "public_gateway"}
    }


def _capture_loguru(func) -> list[str]:
    """Run func() with a temporary loguru sink and return captured messages."""
    from loguru import logger

    messages: list[str] = []
    sink_id = logger.add(lambda m: messages.append(m.record["message"]), level="WARNING")
    try:
        func()
    finally:
        logger.remove(sink_id)
    return messages


def test_override_conflict_warns_on_full_profile(tmp_path):
    cfg = _write_yaml(tmp_path, "tools:\n  profile: full\n")
    msgs = _capture_loguru(lambda: _gateway_profile_override(config_path=str(cfg)))
    # The loud conflict warning names the fix, not the soft "已收紧" note.
    assert any(
        "配置冲突" in m and "artifact_create" in m and "不要" in m and "write_file/exec" in m
        for m in msgs
    )


def test_override_soft_note_when_no_broad_profile(tmp_path):
    cfg = _write_yaml(tmp_path, "tools:\n  profile: minimal\n")
    msgs = _capture_loguru(lambda: _gateway_profile_override(config_path=str(cfg)))
    assert not any("配置冲突" in m for m in msgs)
