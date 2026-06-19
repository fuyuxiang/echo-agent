"""Tests for echo_agent.cli.status — configuration summary rendering.

``load_config`` / ``resolve_config_file`` are mocked so no real config file is
read. We assert on captured stdout and on the pure helpers.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from echo_agent.cli import status as status_mod
from echo_agent.cli.colors import Colors

_T = "echo_agent.cli.status"


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


# ── _effective_workspace ──────────────────────────────────────────────────────

def test_effective_workspace_absolute():
    ws = status_mod._effective_workspace("/srv/agent", None)
    assert ws == Path("/srv/agent").resolve()


def test_effective_workspace_override_anchored_at_cwd():
    ws = status_mod._effective_workspace("ignored", Path("/etc/x/echo.yaml"), "rel")
    assert ws == (Path.cwd() / "rel").resolve()


def test_effective_workspace_relative_anchored_at_config_dir(tmp_path):
    cfg = tmp_path / "echo-agent.yaml"
    ws = status_mod._effective_workspace("sub", cfg)
    assert ws == (tmp_path / "sub").resolve()


def test_effective_workspace_relative_no_config_anchored_at_cwd():
    ws = status_mod._effective_workspace("rel2", None)
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
    gateway = SimpleNamespace(enabled=gateway_enabled, host="0.0.0.0", port=9000)
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
    assert "Only CLI channel is active" in out
    assert "Disabled" in out  # gateway off


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
    assert "Enabled on 0.0.0.0:9000" in out


def test_show_status_config_file_missing_on_disk(capsys):
    # resolve returns a path that does not exist -> "(not found)" branch.
    cfg = _fake_config()
    missing = Path("/nonexistent/echo-agent.yaml")
    with patch(f"{_T}.resolve_config_file", return_value=missing), \
         patch(f"{_T}.load_config", return_value=cfg):
        status_mod.show_status()
    assert "(not found)" in capsys.readouterr().out
