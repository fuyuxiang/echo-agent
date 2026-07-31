"""Tests for echo_agent.cli.status — configuration summary rendering.

``load_config`` / ``resolve_config_file`` are mocked so no real config file is
read. We assert on captured stdout and on the pure helpers.
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from echo_agent.cli import runtime_probe
from echo_agent.cli import status as status_mod
from echo_agent.cli.colors import Colors

_T = "echo_agent.cli.status"
# Runtime probing moved into cli.runtime_probe, so the stubs that used to patch
# status' own helpers now patch the probe's.
_P = "echo_agent.cli.runtime_probe"


# ── _provider_credential_status ──────────────────────────────────────────────
# (a few extra cases beyond test_cli_modules to lock the branch table)

def test_credential_status_pool_beats_api_key():
    p = SimpleNamespace(name="openai", credential_pool=["a"], api_key="k")
    text, clr = status_mod._provider_credential_status(p)
    assert "pool" in text
    assert clr == Colors.GREEN


def test_credential_status_aws_alias():
    p = SimpleNamespace(name="AWS", credential_pool=None, api_key="")
    text, clr = status_mod._provider_credential_status(p)
    assert "AWS environment" in text
    assert clr == Colors.CYAN


# ── _resolve_workspace ───────────────────────────────────────────────────────
# Thin wrapper over the authoritative cli.workspace rule; these lock the four
# branches status actually depends on (absolute / override / config-anchored).

def test_resolve_workspace_absolute():
    ws = status_mod._resolve_workspace(SimpleNamespace(workspace="/srv/agent"), None, None)
    assert ws == Path("/srv/agent").resolve()


def test_resolve_workspace_override_anchored_at_cwd():
    ws = status_mod._resolve_workspace(
        SimpleNamespace(workspace="ignored"), Path("/etc/x/echo.yaml"), "rel"
    )
    assert ws == (Path.cwd() / "rel").resolve()


def test_resolve_workspace_relative_anchored_at_config_dir(tmp_path):
    cfg = tmp_path / "echo-agent.yaml"
    ws = status_mod._resolve_workspace(SimpleNamespace(workspace="sub"), cfg, None)
    assert ws == (tmp_path / "sub").resolve()


def test_resolve_workspace_relative_no_config_anchored_at_cwd():
    ws = status_mod._resolve_workspace(SimpleNamespace(workspace="rel2"), None, None)
    assert ws == (Path.cwd() / "rel2").resolve()


# ── show_status ───────────────────────────────────────────────────────────────

def _fake_config(providers=None, default_model="gpt-4o",
                 channel_overrides=None, gateway_enabled=False,
                 workspace="/ws"):
    channels = SimpleNamespace()
    # Default everything to None so only declared channels render.
    names = [
        "cli", "webhook", "cron", "telegram", "discord", "slack",
        "whatsapp", "weixin", "qqbot", "feishu", "dingtalk",
        "email", "wecom", "matrix",
    ]
    for n in names:
        setattr(channels, n, None)
    for n, enabled in (channel_overrides or {}).items():
        setattr(channels, n, SimpleNamespace(enabled=enabled))
    models = SimpleNamespace(providers=providers or [], default_model=default_model)
    gateway = SimpleNamespace(enabled=gateway_enabled, host="0.0.0.0", port=58123)
    return SimpleNamespace(
        models=models, channels=channels, gateway=gateway, workspace=workspace,
    )


def test_show_status_no_config_file(capsys):
    cfg = _fake_config()
    with patch(f"{_T}.resolve_config_file", return_value=None), \
         patch(f"{_T}.load_config", return_value=cfg):
        status_mod.show_status()
    out = capsys.readouterr().out
    assert "not found" in out
    assert "Echo Agent Status" in out
    assert "No providers configured" in out
    # No channel is enabled in the fake config -> report the truth, not the old
    # "Only CLI channel is active by default" claim.
    assert "No channels enabled" in out
    assert "Disabled in config" in out  # gateway off


def test_show_status_with_providers_and_channels(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("models: {}\n", encoding="utf-8")
    provider = SimpleNamespace(
        name="openai", models=["gpt-4o", "gpt-4o-mini"],
        credential_pool=None, api_key="secret",
    )
    cfg = _fake_config(
        providers=[provider],
        channel_overrides={"telegram": True, "discord": False},
        gateway_enabled=True,
    )
    with patch(f"{_T}.resolve_config_file", return_value=cfg_file), \
         patch(f"{_T}.load_config", return_value=cfg):
        status_mod.show_status(workspace=str(tmp_path))
    out = capsys.readouterr().out
    assert "openai" in out
    assert "gpt-4o" in out
    assert "telegram" in out
    assert "Enabled on 0.0.0.0:58123" in out


def test_show_status_config_file_missing_on_disk(capsys):
    # resolve returns a path that does not exist -> "(not found)" branch.
    cfg = _fake_config()
    missing = Path("/nonexistent/echo-agent.yaml")
    with patch(f"{_T}.resolve_config_file", return_value=missing), \
         patch(f"{_T}.load_config", return_value=cfg):
        status_mod.show_status()
    assert "(not found)" in capsys.readouterr().out


# ── channel summary: no longer contradictory ─────────────────────────────────

def test_channel_summary_none_enabled():
    assert "No channels enabled" in status_mod._channel_summary([])


def test_channel_summary_cli_only():
    assert status_mod._channel_summary(["cli"]) == "Only the CLI channel is enabled"


def test_channel_summary_cli_disabled_reports_truth():
    # cli disabled but telegram on -> must NOT claim CLI is active.
    summary = status_mod._channel_summary(["telegram"])
    assert "CLI" not in summary
    assert "telegram" in summary


def test_status_does_not_lie_when_cli_disabled(capsys):
    cfg = _fake_config(
        providers=[SimpleNamespace(name="openai", models=["gpt-4o"],
                                   credential_pool=None, api_key="k")],
        channel_overrides={"cli": False, "telegram": True},
    )
    with patch(f"{_T}.resolve_config_file", return_value=None), \
         patch(f"{_T}.load_config", return_value=cfg):
        status_mod.show_status()
    out = capsys.readouterr().out
    assert "Only CLI channel is active by default" not in out
    assert "telegram" in out


# ── TCP probe ─────────────────────────────────────────────────────────────────
# The probe itself now lives in cli.runtime_probe (shared with doctor and the
# cli diagnostics); status calls it instead of carrying its own copy. These two
# keep exercising it against a real loopback socket.

def test_tcp_listening_detects_open_port():
    import socket as _socket

    srv = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]
    try:
        assert runtime_probe.tcp_listening("127.0.0.1", port) is True
    finally:
        srv.close()
    # After close the port should no longer accept connections.
    assert runtime_probe.tcp_listening("127.0.0.1", port) is False


def test_tcp_listening_zero_port_is_false():
    assert runtime_probe.tcp_listening("127.0.0.1", 0) is False


# ── --json output + exit codes ────────────────────────────────────────────────

def _healthy_cfg():
    return _fake_config(
        providers=[SimpleNamespace(name="openai", models=["gpt-4o"],
                                   credential_pool=None, api_key="k")],
        channel_overrides={"telegram": True},
        gateway_enabled=False,
    )


def test_status_json_structure_and_no_ansi(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("models: {}\n", encoding="utf-8")
    cfg = _healthy_cfg()
    with patch(f"{_T}.resolve_config_file", return_value=cfg_file), \
         patch(f"{_T}.load_config", return_value=cfg), \
         patch(f"{_P}._read_endpoint", return_value=None), \
         patch(f"{_P}._detect_backend", return_value=None), \
         patch(f"{_T}._health_probes", return_value=None):
        rc = status_mod.show_status(workspace=str(tmp_path), as_json=True)
    out = capsys.readouterr().out
    assert "\033[" not in out  # JSON mode forces color off
    data = json.loads(out)
    assert data["config_file_exists"] is True
    assert data["providers"][0]["name"] == "openai"
    assert data["channels"]["enabled"] == ["telegram"]
    assert set(data["gateway"]) >= {"enabled", "listening", "running_pid", "running_port"}
    assert rc == 0  # config present, provider present, gateway disabled


def test_status_exit_code_no_config_is_nonzero(capsys):
    cfg = _fake_config()
    with patch(f"{_T}.resolve_config_file", return_value=None), \
         patch(f"{_T}.load_config", return_value=cfg), \
         patch(f"{_T}._health_probes", return_value=None):
        rc = status_mod.show_status()
    assert rc == 1  # no config file + no providers


def test_status_exit_code_enabled_gateway_not_listening(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("models: {}\n", encoding="utf-8")
    cfg = _fake_config(
        providers=[SimpleNamespace(name="openai", models=["gpt-4o"],
                                   credential_pool=None, api_key="k")],
        gateway_enabled=True,
    )
    with patch(f"{_T}.resolve_config_file", return_value=cfg_file), \
         patch(f"{_T}.load_config", return_value=cfg), \
         patch(f"{_P}._read_endpoint", return_value=None), \
         patch(f"{_P}.tcp_listening", return_value=False), \
         patch(f"{_P}._detect_backend", return_value=None), \
         patch(f"{_T}._health_probes", return_value=None):
        rc = status_mod.show_status(workspace=str(tmp_path))
    assert rc == 1


def test_status_exit_code_failing_health_probe(tmp_path):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("models: {}\n", encoding="utf-8")
    cfg = _healthy_cfg()
    probes = [{"name": "storage", "status": "fail", "detail": "unwritable"}]
    with patch(f"{_T}.resolve_config_file", return_value=cfg_file), \
         patch(f"{_T}.load_config", return_value=cfg), \
         patch(f"{_P}._read_endpoint", return_value=None), \
         patch(f"{_P}._detect_backend", return_value=None), \
         patch(f"{_T}._health_probes", return_value=probes):
        rc = status_mod.show_status(workspace=str(tmp_path))
    assert rc == 1


def test_status_runtime_endpoint_shown(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("models: {}\n", encoding="utf-8")
    # Gateway on: the recorded endpoint only describes a gateway that is meant
    # to be serving, so the probe does not read it when the component is off.
    cfg = _fake_config(
        providers=[SimpleNamespace(name="openai", models=["gpt-4o"],
                                   credential_pool=None, api_key="k")],
        channel_overrides={"telegram": True},
        gateway_enabled=True,
    )
    endpoint = {"pid": 4242, "port": 59999, "host": "127.0.0.1"}
    with patch(f"{_T}.resolve_config_file", return_value=cfg_file), \
         patch(f"{_T}.load_config", return_value=cfg), \
         patch(f"{_P}._read_endpoint", return_value=endpoint), \
         patch(f"{_P}.tcp_listening", return_value=True), \
         patch(f"{_P}._detect_backend", return_value=None), \
         patch(f"{_T}._health_probes", return_value=None):
        status_mod.show_status(workspace=str(tmp_path))
    out = capsys.readouterr().out
    assert "4242" in out
    assert "59999" in out

