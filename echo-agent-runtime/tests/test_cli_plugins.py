"""Tests for echo_agent.cli.plugins_cmd — plugin management subcommands.

Config loading, plugin discovery and module loading are mocked. The deny-list
toggle test uses a real temp YAML file but no real plugins are imported.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

import yaml

from echo_agent.cli import plugins_cmd

_T = "echo_agent.cli.plugins_cmd"


def _make_record(name="demo", version="1.0.0", description="A demo plugin",
                 author="me", kind="tool", source="user-dir", path="/p/demo",
                 requires_env=None, tools=None, hooks=None, depends_on=None,
                 config_key=""):
    provides = SimpleNamespace(tools=tools or [], hooks=hooks or [])
    manifest = SimpleNamespace(
        name=name, version=version, description=description, author=author,
        kind=kind, requires_env=requires_env or [], provides=provides,
        depends_on=depends_on or [], config_key=config_key,
    )
    return SimpleNamespace(manifest=manifest, source=source, path=path)


def _fake_config(deny=None, allow=None, extra_dirs=None):
    plugins = SimpleNamespace(deny=deny or [], allow=allow or [], extra_dirs=extra_dirs or [])
    return SimpleNamespace(plugins=plugins, workspace="/ws")


# ── run_plugin_command dispatch ──────────────────────────────────────────────

def test_dispatch_list_calls_list():
    with patch(f"{_T}._list_plugins") as fn:
        plugins_cmd.run_plugin_command("list")
    fn.assert_called_once()


def test_dispatch_check_calls_check():
    # check 现在把 _check_plugins 的退出码原样返回,0 表示全部通过;
    # sys.exit 由 __main__ 统一负责。
    with patch(f"{_T}._check_plugins", return_value=0) as fn:
        rc = plugins_cmd.run_plugin_command("check")
    fn.assert_called_once()
    assert rc == 0


def test_dispatch_info_requires_name(capsys):
    rc = plugins_cmd.run_plugin_command("info", name="")
    assert rc == 1
    assert "Usage" in capsys.readouterr().out


def test_dispatch_info_with_name():
    with patch(f"{_T}._show_plugin_info") as fn:
        plugins_cmd.run_plugin_command("info", name="demo")
    fn.assert_called_once()


def test_dispatch_enable_requires_name(capsys):
    assert plugins_cmd.run_plugin_command("enable", name="") == 1
    assert "Usage" in capsys.readouterr().out


def test_dispatch_enable_with_name():
    with patch(f"{_T}._toggle_plugin") as fn:
        plugins_cmd.run_plugin_command("enable", name="demo")
    fn.assert_called_once()
    assert fn.call_args.kwargs["enable"] is True


def test_dispatch_disable_with_name():
    with patch(f"{_T}._toggle_plugin") as fn:
        plugins_cmd.run_plugin_command("disable", name="demo")
    assert fn.call_args.kwargs["enable"] is False


def test_dispatch_unknown_action_exits(capsys):
    rc = plugins_cmd.run_plugin_command("frobnicate")
    assert rc == 1
    out = capsys.readouterr().out
    assert "Unknown plugin action" in out


# ── _get_config_and_workspace ─────────────────────────────────────────────────

def test_get_config_and_workspace_resolves(tmp_path):
    cfg = _fake_config()
    cfg.workspace = str(tmp_path)
    with patch("echo_agent.config.loader.resolve_config_file", return_value=None), \
         patch("echo_agent.config.loader.load_config", return_value=cfg):
        out_cfg, ws = plugins_cmd._get_config_and_workspace(None, None)
    assert out_cfg is cfg
    assert ws == tmp_path.resolve()


# ── _list_plugins ─────────────────────────────────────────────────────────────

def test_list_plugins_empty(capsys):
    cfg = _fake_config()
    with patch(f"{_T}._get_config_and_workspace", return_value=(cfg, Path("/ws"))), \
         patch("echo_agent.plugins.loader.discover_all", return_value=[]):
        plugins_cmd._list_plugins(None, None)
    out = capsys.readouterr().out
    assert "No plugins discovered" in out
    assert "Search locations" in out


def test_list_plugins_status_markers(capsys):
    cfg = _fake_config(deny=["denied"], allow=["available"])
    records = [
        _make_record(name="available"),
        _make_record(name="denied"),
        _make_record(name="other"),  # allow set present but not listed -> filtered
    ]
    with patch(f"{_T}._get_config_and_workspace", return_value=(cfg, Path("/ws"))), \
         patch("echo_agent.plugins.loader.discover_all", return_value=records):
        plugins_cmd._list_plugins(None, None)
    out = capsys.readouterr().out
    assert "available" in out
    assert "disabled" in out
    assert "filtered" in out
    assert "3 discovered" in out


# ── _show_plugin_info ─────────────────────────────────────────────────────────

def test_show_plugin_info_not_found(capsys):
    cfg = _fake_config()
    with patch(f"{_T}._get_config_and_workspace", return_value=(cfg, Path("/ws"))), \
         patch("echo_agent.plugins.loader.discover_all", return_value=[]):
        rc = plugins_cmd._show_plugin_info("ghost", None, None)
    assert rc == 1
    assert "not found" in capsys.readouterr().out


def test_show_plugin_info_renders_all_fields(capsys):
    cfg = _fake_config()
    rec = _make_record(
        name="demo", requires_env=["API_KEY"], tools=["t1"], hooks=["h1"],
        depends_on=["base"], config_key="demo",
    )
    with patch(f"{_T}._get_config_and_workspace", return_value=(cfg, Path("/ws"))), \
         patch("echo_agent.plugins.loader.discover_all", return_value=[rec]), \
         patch("echo_agent.plugins.manifest.check_required_env", return_value=["API_KEY"]):
        plugins_cmd._show_plugin_info("demo", None, None)
    out = capsys.readouterr().out
    assert "Plugin: demo" in out
    assert "Requires env: API_KEY" in out
    assert "Provides tools: t1" in out
    assert "Provides hooks: h1" in out
    assert "Depends on: base" in out
    assert "plugins.config.demo" in out
    assert "WARNING: Missing env vars" in out


# ── _toggle_plugin ────────────────────────────────────────────────────────────

def test_toggle_no_config_file_exits(capsys):
    with patch("echo_agent.config.loader.resolve_config_file", return_value=None):
        rc = plugins_cmd._toggle_plugin("demo", enable=False, config_path=None)
    assert rc == 1
    assert "No config file found" in capsys.readouterr().out


def test_toggle_disable_adds_to_deny(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("plugins:\n  deny: []\n", encoding="utf-8")
    with patch("echo_agent.config.loader.resolve_config_file", return_value=cfg_file):
        plugins_cmd._toggle_plugin("demo", enable=False, config_path=None)
    data = yaml.safe_load(cfg_file.read_text(encoding="utf-8"))
    assert "demo" in data["plugins"]["deny"]
    assert "disabled" in capsys.readouterr().out


def test_toggle_disable_already_denied_noop(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("plugins:\n  deny: [demo]\n", encoding="utf-8")
    with patch("echo_agent.config.loader.resolve_config_file", return_value=cfg_file):
        plugins_cmd._toggle_plugin("demo", enable=False, config_path=None)
    assert "already disabled" in capsys.readouterr().out


def test_toggle_enable_removes_from_deny(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("plugins:\n  deny: [demo]\n", encoding="utf-8")
    with patch("echo_agent.config.loader.resolve_config_file", return_value=cfg_file):
        plugins_cmd._toggle_plugin("demo", enable=True, config_path=None)
    data = yaml.safe_load(cfg_file.read_text(encoding="utf-8"))
    assert "demo" not in data["plugins"]["deny"]
    assert "enabled" in capsys.readouterr().out


def test_toggle_enable_not_in_deny_noop(tmp_path, capsys):
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("plugins:\n  deny: []\n", encoding="utf-8")
    with patch("echo_agent.config.loader.resolve_config_file", return_value=cfg_file):
        plugins_cmd._toggle_plugin("demo", enable=True, config_path=None)
    # 不在 deny 且无 allow 白名单需补入,视为已启用。
    assert "already enabled" in capsys.readouterr().out


def test_toggle_enable_adds_to_nonempty_allow(tmp_path, capsys):
    # 白名单非空时,enable 除了移出 deny,还应把插件补进 allow,
    # 否则运行期仍被白名单挡下。
    cfg_file = tmp_path / "echo-agent.yaml"
    cfg_file.write_text("plugins:\n  allow: [other]\n  deny: [demo]\n", encoding="utf-8")
    with patch("echo_agent.config.loader.resolve_config_file", return_value=cfg_file):
        plugins_cmd._toggle_plugin("demo", enable=True, config_path=None)
    import yaml
    data = yaml.safe_load(cfg_file.read_text(encoding="utf-8"))
    assert "demo" not in data["plugins"]["deny"]
    assert "demo" in data["plugins"]["allow"]
    assert "enabled" in capsys.readouterr().out


# ── _check_plugins ────────────────────────────────────────────────────────────

def test_check_plugins_ok_fail_skip(capsys):
    cfg = _fake_config()
    ok = _make_record(name="ok")
    skip = _make_record(name="skip", requires_env=["MISSING"])
    fail = _make_record(name="fail")

    def _check_env(manifest):
        return ["MISSING"] if manifest.name == "skip" else []

    def _load(record):
        if record.manifest.name == "fail":
            raise RuntimeError("boom")
        return MagicMock()

    with patch(f"{_T}._get_config_and_workspace", return_value=(cfg, Path("/ws"))), \
         patch("echo_agent.plugins.loader.discover_all", return_value=[ok, skip, fail]), \
         patch("echo_agent.plugins.loader.topological_sort", side_effect=lambda r: r), \
         patch("echo_agent.plugins.loader.load_plugin_module", side_effect=_load), \
         patch("echo_agent.plugins.manifest.check_required_env", side_effect=_check_env):
        plugins_cmd._check_plugins(None, None)
    out = capsys.readouterr().out
    assert "OK    ok" in out
    assert "SKIP  skip" in out
    assert "FAIL  fail" in out
    assert "1 OK, 2 failed/skipped" in out
