"""Config diagnostics keep credentials private and malformed YAML actionable."""

from echo_agent.cli.config_cmd import run_config_command


def test_explain_redacts_current_secret(tmp_path, capsys):
    config = tmp_path / "echo-agent.yaml"
    config.write_text("channels:\n  telegram:\n    token: diagnostic-secret-123\n")
    assert run_config_command("explain", "channels.telegram.token", config_path=str(config)) == 0
    output = capsys.readouterr().out
    assert "diagnostic-secret-123" not in output
    assert "****" in output


def test_validate_malformed_yaml_returns_readable_nonzero(tmp_path, capsys):
    config = tmp_path / "echo-agent.yaml"
    config.write_text("models: [unterminated\n")
    assert run_config_command("validate", config_path=str(config)) == 1
    output = capsys.readouterr().out
    assert "Invalid YAML" in output
    assert "line" in output
