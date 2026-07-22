"""Tests for echo_agent.dependencies.cli and skill_require."""

from __future__ import annotations

from unittest.mock import patch

import pytest

from echo_agent.dependencies import cli as deps_cli
from echo_agent.dependencies import skill_require as sr
from echo_agent.dependencies import lazy_deps as ld


# ── deps CLI: _status ────────────────────────────────────────────────────────


def test_status_prints_ready_and_missing(capsys):
    report = {
        "skill.calculator": {"available": True, "missing": [], "command": None},
        "skill.excel-author": {
            "available": False,
            "missing": ["openpyxl>=3.1"],
            "command": "uv pip install 'openpyxl>=3.1'",
        },
    }
    with patch.object(deps_cli, "_status", deps_cli._status):
        with patch("echo_agent.dependencies.lazy_deps.check_all_features", return_value=report):
            deps_cli._status()
    out = capsys.readouterr().out
    assert "Ready (1)" in out
    assert "Missing (1)" in out
    assert "openpyxl>=3.1" in out
    assert "Total: 2 features" in out


# ── deps CLI: _install ───────────────────────────────────────────────────────


def test_install_unknown_feature_exits(capsys):
    with pytest.raises(SystemExit) as exc:
        deps_cli._install("skill.nope")
    assert exc.value.code == 1
    assert "Unknown feature" in capsys.readouterr().out


def test_install_already_satisfied(capsys):
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=True):
        deps_cli._install("skill.excel-author")
    assert "already satisfied" in capsys.readouterr().out


def test_install_success(capsys):
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=False), \
         patch("echo_agent.dependencies.lazy_deps.ensure", return_value=None):
        deps_cli._install("skill.excel-author")
    assert "installed successfully" in capsys.readouterr().out


def test_install_failure_exits(capsys):
    err = ld.FeatureUnavailable("skill.excel-author", ("openpyxl>=3.1",), "boom")
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=False), \
         patch("echo_agent.dependencies.lazy_deps.ensure", side_effect=err):
        with pytest.raises(SystemExit) as exc:
            deps_cli._install("skill.excel-author")
    assert exc.value.code == 1


# ── deps CLI: _install_all ───────────────────────────────────────────────────


def test_install_all_nothing_to_do(capsys):
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=True):
        deps_cli._install_all()
    assert "already satisfied" in capsys.readouterr().out


def test_install_all_proceeds(capsys):
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=False), \
         patch("echo_agent.dependencies.lazy_deps.ensure", return_value=None), \
         patch("builtins.input", return_value="y"):
        deps_cli._install_all()
    out = capsys.readouterr().out
    assert "Will install" in out
    assert "Done:" in out


def test_install_all_cancelled(capsys):
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=False), \
         patch("builtins.input", return_value="no"):
        deps_cli._install_all()
    assert "Cancelled" in capsys.readouterr().out


def test_install_all_handles_failures(capsys):
    err = ld.FeatureUnavailable("skill.tts-voice", ("edge-tts>=7.0",), "nope")
    with patch("echo_agent.dependencies.lazy_deps.is_available", return_value=False), \
         patch("echo_agent.dependencies.lazy_deps.ensure", side_effect=err), \
         patch("builtins.input", return_value=""):
        deps_cli._install_all()
    out = capsys.readouterr().out
    assert "[FAIL]" in out


# ── deps CLI: _refresh ───────────────────────────────────────────────────────


def test_refresh_no_active(capsys):
    with patch("echo_agent.dependencies.lazy_deps.refresh_active_features", return_value={}):
        deps_cli._refresh()
    assert "No features" in capsys.readouterr().out


def test_refresh_with_results(capsys):
    results = {"skill.a": "current", "skill.b": "refreshed", "skill.c": "failed: x"}
    with patch("echo_agent.dependencies.lazy_deps.refresh_active_features", return_value=results):
        deps_cli._refresh()
    out = capsys.readouterr().out
    assert "[OK]" in out and "[UP]" in out and "[!!]" in out


# ── deps CLI: main dispatch ──────────────────────────────────────────────────


def test_main_status(capsys):
    with patch.object(deps_cli, "_status") as fn:
        deps_cli.main(["status"])
    fn.assert_called_once()


def test_main_install_with_feature():
    with patch.object(deps_cli, "_install") as fn:
        deps_cli.main(["install", "skill.excel-author"])
    fn.assert_called_once_with("skill.excel-author")


def test_main_install_all():
    with patch.object(deps_cli, "_install_all") as fn:
        deps_cli.main(["install"])
    fn.assert_called_once()


def test_main_refresh():
    with patch.object(deps_cli, "_refresh") as fn:
        deps_cli.main(["refresh"])
    fn.assert_called_once()


def test_main_no_command_prints_help(capsys):
    deps_cli.main([])
    assert "usage" in capsys.readouterr().out.lower()


# ── deps CLI: --json output + exit codes ─────────────────────────────────────


def test_status_json_all_ready_exit_zero(capsys):
    report = {"skill.calc": {"available": True, "missing": [], "command": None}}
    with patch("echo_agent.dependencies.lazy_deps.check_all_features", return_value=report):
        rc = deps_cli._status(as_json=True)
    out = capsys.readouterr().out
    assert "\033[" not in out
    import json as _json
    data = _json.loads(out)
    assert data["ready"] == ["skill.calc"]
    assert data["missing"] == []
    assert rc == 0


def test_status_json_missing_exit_nonzero(capsys):
    report = {
        "skill.calc": {"available": True, "missing": [], "command": None},
        "skill.excel": {"available": False, "missing": ["openpyxl>=3.1"],
                        "command": "uv pip install 'openpyxl>=3.1'"},
    }
    with patch("echo_agent.dependencies.lazy_deps.check_all_features", return_value=report):
        rc = deps_cli._status(as_json=True)
    import json as _json
    data = _json.loads(capsys.readouterr().out)
    assert data["missing"][0]["feature"] == "skill.excel"
    assert data["missing"][0]["packages"] == ["openpyxl>=3.1"]
    assert rc == 1


def test_main_status_json_returns_exit_code():
    report = {"skill.excel": {"available": False, "missing": ["x"], "command": "c"}}
    with patch("echo_agent.dependencies.lazy_deps.check_all_features", return_value=report):
        rc = deps_cli.main(["status", "--json"])
    assert rc == 1


# ── skill_require.require ────────────────────────────────────────────────────


def test_require_success(monkeypatch):
    monkeypatch.setattr(sr, "_is_interactive", lambda: True)
    with patch("echo_agent.dependencies.lazy_deps.ensure", return_value=None) as fn:
        sr.require("skill.excel-author", prompt=True)
    fn.assert_called_once()


def test_require_autodetect_prompt(monkeypatch):
    monkeypatch.setattr(sr, "_is_interactive", lambda: False)
    with patch("echo_agent.dependencies.lazy_deps.ensure", return_value=None) as fn:
        sr.require("skill.excel-author")  # prompt=None → autodetect
    # prompt should resolve to False for non-interactive
    assert fn.call_args.kwargs["prompt"] is False


def test_require_interactive_failure_exits(monkeypatch):
    monkeypatch.setattr(sr, "_is_interactive", lambda: True)
    with patch("echo_agent.dependencies.lazy_deps.ensure", side_effect=RuntimeError("boom")):
        with pytest.raises(SystemExit) as exc:
            sr.require("skill.excel-author", prompt=True)
    assert "Dependency error" in str(exc.value)


def test_require_noninteractive_failure_friendly_exit(monkeypatch, capsys):
    monkeypatch.setattr(sr, "_is_interactive", lambda: False)
    with patch("echo_agent.dependencies.lazy_deps.ensure", side_effect=RuntimeError("boom")):
        with pytest.raises(SystemExit) as exc:
            sr.require("skill.excel-author", prompt=False)
    assert exc.value.code == 1
    assert "requires additional packages" in capsys.readouterr().out


# ── skill_require._fallback_check ────────────────────────────────────────────


def test_require_falls_back_on_importerror(monkeypatch):
    # ensure() raising ImportError routes to _fallback_check.
    monkeypatch.setattr(sr, "_is_interactive", lambda: False)
    with patch("echo_agent.dependencies.lazy_deps.ensure", side_effect=ImportError):
        called = {}
        monkeypatch.setattr(sr, "_fallback_check", lambda f: called.setdefault("f", f))
        sr.require("skill.excel-author", prompt=False)
    assert called["f"] == "skill.excel-author"


def test_fallback_check_missing_noninteractive(monkeypatch, capsys):
    monkeypatch.setattr(sr, "_is_interactive", lambda: False)
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    with pytest.raises(SystemExit):
        sr._fallback_check("skill.excel-author")
    assert "requires additional packages" in capsys.readouterr().out


def test_fallback_check_missing_interactive(monkeypatch):
    monkeypatch.setattr(sr, "_is_interactive", lambda: True)
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    with pytest.raises(SystemExit) as exc:
        sr._fallback_check("skill.excel-author")
    assert "Missing" in str(exc.value)


def test_fallback_check_all_satisfied_noop(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: True)
    # nothing missing → returns without exit
    sr._fallback_check("skill.excel-author")


# ── skill_require._channel_friendly_exit ─────────────────────────────────────


def test_channel_friendly_exit_includes_command(capsys):
    with patch(
        "echo_agent.dependencies.lazy_deps.feature_install_command",
        return_value="uv pip install 'openpyxl>=3.1'",
    ):
        with pytest.raises(SystemExit):
            sr._channel_friendly_exit("skill.excel-author", "Missing: openpyxl")
    out = capsys.readouterr().out
    assert "Administrator can run" in out
    assert "allow_lazy_installs" in out


# ── skill_require.require_any ─────────────────────────────────────────────────


def test_require_any_present(monkeypatch):
    # find_spec returns truthy for the first package → returns without exit
    monkeypatch.setattr(sr, "_is_interactive", lambda: False)
    with patch("importlib.util.find_spec", return_value=object()):
        sr.require_any("pymupdf", "PyMuPDF")  # should not raise


def test_require_any_missing_interactive(monkeypatch):
    monkeypatch.setattr(sr, "_is_interactive", lambda: True)
    with patch("importlib.util.find_spec", return_value=None):
        with pytest.raises(SystemExit) as exc:
            sr.require_any("pkga", "pkgb")
    assert "pip install pkga" in str(exc.value)


def test_require_any_missing_noninteractive(monkeypatch, capsys):
    monkeypatch.setattr(sr, "_is_interactive", lambda: False)
    with patch("importlib.util.find_spec", return_value=None):
        with pytest.raises(SystemExit) as exc:
            sr.require_any("pkga", "pkgb")
    assert exc.value.code == 1
    assert "requires one of" in capsys.readouterr().out
