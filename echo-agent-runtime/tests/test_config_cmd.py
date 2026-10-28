"""Tests for the `config` CLI command (dump/explain/validate)."""
from __future__ import annotations

from echo_agent.cli.config_cmd import known_paths, redact, run_config_command


def test_redact_masks_secret_keys():
    data = {"models": {"providers": [{"name": "openai", "apiKey": "sk-secret"}]},
            "channels": {"telegram": {"token": "abc"}}}
    out = redact(data)
    assert out["models"]["providers"][0]["apiKey"] == "****"
    assert out["channels"]["telegram"]["token"] == "****"
    # 非敏感字段保留
    assert out["models"]["providers"][0]["name"] == "openai"


def test_redact_keeps_empty_values():
    out = redact({"channels": {"telegram": {"token": ""}}})
    assert out["channels"]["telegram"]["token"] == ""


def test_redact_masks_credential_pool_list():
    # models.providers[].credentialPool 是 list[str] 的轮换密钥池,必须打码
    data = {"models": {"providers": [
        {"name": "openai", "credentialPool": ["sk-a", "sk-b"]}
    ]}}
    out = redact(data)
    masked = out["models"]["providers"][0]["credentialPool"]
    assert masked == ["****"] or all(x == "****" for x in masked)
    # 非敏感字段保留
    assert out["models"]["providers"][0]["name"] == "openai"


def test_redact_masks_mcp_auth_string():
    # tools.mcpServers{}.auth 是 MCP 认证凭据(str),必须打码
    data = {"tools": {"mcpServers": {"fs": {"auth": "bearer-secret"}}}}
    out = redact(data)
    assert out["tools"]["mcpServers"]["fs"]["auth"] == "****"


def test_redact_keeps_empty_credential_pool():
    # 空列表不打码,保持现有行为
    out = redact({"models": {"providers": [{"credentialPool": []}]}})
    assert out["models"]["providers"][0]["credentialPool"] == []


def test_redact_does_not_mask_gateway_auth_block():
    # gateway.auth 是字典块,其下 mode/allowedUsers 非凭据,不得误打码
    data = {"gateway": {"auth": {
        "mode": "allowlist",
        "allowedUsers": ["alice", "bob"],
    }}}
    out = redact(data)
    assert out["gateway"]["auth"]["mode"] == "allowlist"
    assert out["gateway"]["auth"]["allowedUsers"] == ["alice", "bob"]


def test_dump_prints_yaml(capsys):
    rc = run_config_command("dump")
    assert rc == 0
    out = capsys.readouterr().out
    assert "workspace" in out


def test_explain_effective_field(capsys):
    rc = run_config_command("explain", "compression.triggerRatio")
    assert rc == 0
    out = capsys.readouterr().out
    assert "0.7" in out  # 默认值
    assert "effective" in out.lower() or "生效" in out


def test_explain_dead_field_warns(capsys):
    # Task 7 后 schema 无 dead 字段;若将来重新引入,explain 必须提示未生效。
    # 动态选取一个 dead 字段;没有则跳过(机制仍在 config_cmd.py 中受守护)。
    import pytest

    from echo_agent.config.metadata import iter_fields
    from echo_agent.config.schema import Config
    dead = [f for f in iter_fields(Config) if f.extra.get("status") == "dead"]
    if not dead:
        pytest.skip("no dead fields remain after Task 7")
    run_config_command("explain", dead[0].snake_path)
    out = capsys.readouterr().out
    assert "未生效" in out or "not in effect" in out.lower() or "dead" in out.lower()


def test_explain_unknown_key(capsys):
    rc = run_config_command("explain", "memory.nonsenseField")
    assert rc != 0
    out = capsys.readouterr().out
    assert "未知" in out or "unknown" in out.lower()


def test_explain_group_key_lists_children(capsys):
    # 分组级 key(如 gateway)匹配不到叶子字段,应回退列出其下子项而非报未知。
    rc = run_config_command("explain", "gateway")
    assert rc == 0
    out = capsys.readouterr().out
    assert "分组" in out or "group" in out.lower()
    assert "gateway.port" in out
    assert "未知" not in out


def test_explain_snake_group_key_lists_children(capsys):
    # snake_case 分组前缀同样支持。
    rc = run_config_command("explain", "gateway")
    assert rc == 0
    out = capsys.readouterr().out
    assert "gateway.enabled" in out


def test_known_paths_contains_both_styles():
    paths = known_paths()
    assert "memory.archivalThreshold" in paths
    assert "memory.archival_threshold" in paths


def test_validate_clean_config(tmp_path, capsys):
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("memory:\n  enabled: true\n", encoding="utf-8")
    rc = run_config_command("validate", config_path=str(cfg))
    assert rc == 0


def test_validate_reports_unknown_field(tmp_path, capsys):
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("memory:\n  enabledd: true\n", encoding="utf-8")
    rc = run_config_command("validate", config_path=str(cfg))
    out = capsys.readouterr().out
    assert rc != 0
    assert "enabledd" in out


def test_validate_warns_dead_field(tmp_path, capsys):
    # Task 7 后无 dead 字段;若将来重新引入,validate 对显式设置的死字段应警告未生效。
    import pytest

    from echo_agent.config.metadata import iter_fields
    from echo_agent.config.schema import Config
    dead = [f for f in iter_fields(Config) if f.extra.get("status") == "dead"]
    if not dead:
        pytest.skip("no dead fields remain after Task 7")
    info = dead[0]
    parts = info.snake_path.split(".")
    cfg = tmp_path / "echo-agent.yaml"
    # 构造一个把该死字段写出来的最小 YAML
    yaml_lines = []
    for i, p in enumerate(parts[:-1]):
        yaml_lines.append("  " * i + f"{p}:")
    yaml_lines.append("  " * (len(parts) - 1) + f"{parts[-1]}: 0")
    cfg.write_text("\n".join(yaml_lines) + "\n", encoding="utf-8")
    run_config_command("validate", config_path=str(cfg))
    out = capsys.readouterr().out
    assert parts[-1] in out and ("未生效" in out or "not in effect" in out.lower())


def test_parser_accepts_config_subcommand():
    from echo_agent.__main__ import _build_parser
    parser = _build_parser()
    args = parser.parse_args(["config", "explain", "memory.enabled"])
    assert args.command == "config"
    assert args.action == "explain"
    assert args.key == "memory.enabled"


def test_parser_config_dump_format():
    from echo_agent.__main__ import _build_parser
    parser = _build_parser()
    args = parser.parse_args(["config", "dump", "--format", "json"])
    assert args.action == "dump"
    assert args.format == "json"
