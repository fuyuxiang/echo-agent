"""Tests for echo_agent.cli.workspace — the one authoritative workspace rule
and the gateway runtime-endpoint contract shared with attach / service status."""

from __future__ import annotations

from types import SimpleNamespace

from echo_agent.cli.workspace import (
    clear_runtime_endpoint,
    endpoint_path,
    read_runtime_endpoint,
    resolve_effective_workspace,
    write_runtime_endpoint,
)


def _cfg(workspace: str):
    return SimpleNamespace(workspace=workspace)


# ── resolve_effective_workspace ──────────────────────────────────────────────

def test_relative_workspace_resolves_against_config_dir(tmp_path, monkeypatch):
    # 相对 workspace、无 override:按配置文件所在目录解析,与 cwd 无关。
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)
    config_file = tmp_path / "proj" / "echo-agent.yaml"
    config_file.parent.mkdir(parents=True)
    ws = resolve_effective_workspace(_cfg("./data"), str(config_file), None)
    assert ws == (tmp_path / "proj" / "data").resolve()


def test_override_resolves_against_cwd(tmp_path, monkeypatch):
    # 显式 -w(override):按 shell cwd 解析,忽略配置文件目录。
    monkeypatch.chdir(tmp_path)
    config_file = tmp_path / "proj" / "echo-agent.yaml"
    config_file.parent.mkdir(parents=True)
    ws = resolve_effective_workspace(_cfg("./data"), str(config_file), "custom-ws")
    assert ws == (tmp_path / "custom-ws").resolve()


def test_absolute_workspace_used_verbatim(tmp_path):
    abs_ws = tmp_path / "abs"
    ws = resolve_effective_workspace(_cfg(str(abs_ws)), None, None)
    assert ws == abs_ws.resolve()


def test_no_config_file_relative_falls_back_to_cwd(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    ws = resolve_effective_workspace(_cfg("./data"), None, None)
    assert ws == (tmp_path / "data").resolve()


# ── runtime endpoint roundtrip ───────────────────────────────────────────────

def test_write_read_endpoint_roundtrip(tmp_path):
    write_runtime_endpoint(
        tmp_path, host="127.0.0.1", port=51234, pid=4321, ws_path="/ws"
    )
    assert endpoint_path(tmp_path) == tmp_path / ".echo-agent" / "gateway.json"
    data = read_runtime_endpoint(tmp_path)
    assert data == {"host": "127.0.0.1", "port": 51234, "pid": 4321, "ws_path": "/ws"}


def test_read_endpoint_missing_returns_none(tmp_path):
    assert read_runtime_endpoint(tmp_path) is None


def test_read_endpoint_corrupt_returns_none(tmp_path):
    path = endpoint_path(tmp_path)
    path.parent.mkdir(parents=True)
    path.write_text("{not json", encoding="utf-8")
    assert read_runtime_endpoint(tmp_path) is None


def test_clear_endpoint_removes_file(tmp_path):
    write_runtime_endpoint(tmp_path, host="h", port=1, pid=2, ws_path="/ws")
    clear_runtime_endpoint(tmp_path)
    assert read_runtime_endpoint(tmp_path) is None
    # idempotent: clearing an already-absent file is a no-op.
    clear_runtime_endpoint(tmp_path)


# ── GatewayConfig.port validation ────────────────────────────────────────────

def test_gateway_port_zero_allowed():
    from echo_agent.config.schema import GatewayConfig

    assert GatewayConfig(port=0).port == 0


def test_gateway_port_valid_range_allowed():
    from echo_agent.config.schema import GatewayConfig

    assert GatewayConfig(port=65535).port == 65535


def test_gateway_port_out_of_range_rejected():
    import pytest
    from pydantic import ValidationError

    from echo_agent.config.schema import GatewayConfig

    for bad in (-1, 70000):
        with pytest.raises(ValidationError):
            GatewayConfig(port=bad)

