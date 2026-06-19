"""Smoke + pure-logic tests for echo_agent.cli.setup.

The wizard is 780 lines of interactive I/O; per the test plan we deliberately
do NOT exercise the interactive menu loop. We cover:
  - the section/alias registry
  - pure path/locale helpers
  - _capability_check branch table
  - the headless guidance path of run_setup_wizard (is_interactive False)
  - single-section routing (section= argument) with prompts/save mocked
  - has_any_provider_configured
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

from echo_agent.cli import setup as setup_mod
from echo_agent.cli.i18n import get_locale, set_locale

set_locale("en")
_T = "echo_agent.cli.setup"


@pytest.fixture(autouse=True)
def _restore_locale():
    """Several helpers mutate the process-global locale; restore it so this
    file never leaks "zh" into locale-sensitive tests in other files."""
    saved = get_locale()
    try:
        yield
    finally:
        set_locale(saved)


# ── registry ──────────────────────────────────────────────────────────────────

def test_section_registry_keys_unique():
    keys = [k for k, _ in setup_mod.SETUP_SECTIONS]
    assert len(keys) == len(set(keys))
    assert "model" in keys and "cost" in keys


def test_aliases_map_to_real_sections():
    section_keys = {k for k, _ in setup_mod.SETUP_SECTIONS} | {"doctor"}
    for canonical in setup_mod.SECTION_ALIASES.values():
        assert canonical in section_keys


# ── pure helpers ──────────────────────────────────────────────────────────────

def test_resolve_workspace_default(monkeypatch):
    ws = setup_mod._resolve_workspace({})
    assert ws == Path("~/.echo-agent").expanduser()


def test_resolve_workspace_relative_anchored_at_cwd():
    ws = setup_mod._resolve_workspace({"workspace": "rel"})
    assert ws == (Path.cwd() / "rel").resolve()


def test_resolve_workspace_absolute():
    ws = setup_mod._resolve_workspace({"workspace": "/srv/x"})
    assert ws == Path("/srv/x")


def test_setup_config_target_explicit_path():
    target = setup_mod._setup_config_target(config_path="/tmp/echo.yaml")
    assert target == Path("/tmp/echo.yaml")


def test_setup_config_target_workspace_default(tmp_path):
    with patch(f"{_T}.find_local_config_file", return_value=None):
        target = setup_mod._setup_config_target(workspace=str(tmp_path))
    assert target == tmp_path / "echo-agent.yaml"


def test_load_existing_config_missing_returns_empty():
    with patch(f"{_T}.resolve_config_file", return_value=None):
        cfg, existing = setup_mod._load_existing_config(None, None)
    assert cfg == {}
    assert existing is None


def test_load_existing_config_reads_yaml(tmp_path):
    f = tmp_path / "echo-agent.yaml"
    f.write_text("models:\n  default_model: gpt-4o\n", encoding="utf-8")
    with patch(f"{_T}.resolve_config_file", return_value=f):
        cfg, existing = setup_mod._load_existing_config(None, None)
    assert cfg["models"]["default_model"] == "gpt-4o"
    assert existing == f


def test_resolve_initial_locale_override_wins():
    assert setup_mod._resolve_initial_locale({}, "zh") == "zh"
    set_locale("en")


def test_resolve_initial_locale_saved_pref():
    out = setup_mod._resolve_initial_locale({"ui": {"locale": "zh"}}, None)
    assert out == "zh"
    set_locale("en")


# ── _capability_check ──────────────────────────────────────────────────────────

def test_capability_check_empty_config():
    checks = setup_mod._capability_check({})
    labels = [c[0] for c in checks]
    oks = [c[1] for c in checks]
    # Always returns a fixed-length checklist; first item is the model check.
    assert len(checks) >= 10
    assert all(isinstance(label, str) and label for label in labels)
    assert oks[0] is False  # no providers


def test_capability_check_full_config():
    config = {
        "models": {"providers": [{"name": "openai"}]},
        "channels": {"telegram": {"enabled": True}, "cli": {"enabled": True}},
        "gateway": {"enabled": True, "host": "0.0.0.0", "port": 9000},
        "tools": {"profile": "full", "mcp_servers": {"x": {}}},
        "observability": {"log_level": "DEBUG", "otel_enabled": True,
                          "otel_endpoint": "http://otel:4317"},
    }
    checks = setup_mod._capability_check(config)
    assert checks[0][1] is True  # model present
    # The channel check reports the enabled non-cli channel.
    channel_check = next(c for c in checks if "telegram" in c[2])
    assert channel_check[1] is True


# ── run_setup_wizard ────────────────────────────────────────────────────────────

def test_wizard_headless_prints_guidance(capsys):
    with patch(f"{_T}.is_interactive", return_value=False), \
         patch(f"{_T}._load_existing_config", return_value=({}, None)):
        setup_mod.run_setup_wizard()
    out = capsys.readouterr().out
    # Headless guidance is shown rather than an interactive menu.
    assert out.strip() != ""


def test_wizard_unknown_section_reports(capsys, tmp_path):
    with patch(f"{_T}.is_interactive", return_value=True), \
         patch(f"{_T}._setup_config_target", return_value=tmp_path / "c.yaml"), \
         patch(f"{_T}._load_existing_config", return_value=({}, None)), \
         patch(f"{_T}._print_banner"):
        setup_mod.run_setup_wizard(section="does-not-exist")
    out = capsys.readouterr().out
    assert "Unknown section" in out
    assert "Available" in out


def test_wizard_single_section_runs_and_saves(tmp_path):
    target = tmp_path / "echo-agent.yaml"
    calls = {}

    def _fake_cost(config):
        calls["ran"] = True
        config.setdefault("cost", {})["enabled"] = False

    with patch(f"{_T}.is_interactive", return_value=True), \
         patch(f"{_T}._setup_config_target", return_value=target), \
         patch(f"{_T}._load_existing_config", return_value=({}, None)), \
         patch(f"{_T}._print_banner"), \
         patch(f"{_T}.save_config", return_value=str(target)) as save, \
         patch.object(setup_mod, "SETUP_SECTIONS", [("cost", _fake_cost)]):
        setup_mod.run_setup_wizard(section="cost")
    assert calls.get("ran") is True
    save.assert_called_once()


def test_wizard_doctor_section(tmp_path):
    with patch(f"{_T}.is_interactive", return_value=True), \
         patch(f"{_T}._setup_config_target", return_value=tmp_path / "c.yaml"), \
         patch(f"{_T}._load_existing_config", return_value=({}, None)), \
         patch(f"{_T}._print_banner"), \
         patch(f"{_T}.setup_doctor") as doctor:
        setup_mod.run_setup_wizard(section="doctor")
    doctor.assert_called_once()


# ── has_any_provider_configured ─────────────────────────────────────────────────

def test_has_provider_false_when_no_file():
    with patch(f"{_T}.resolve_config_file", return_value=None):
        assert setup_mod.has_any_provider_configured() is False


def test_has_provider_true_with_providers(tmp_path):
    f = tmp_path / "echo-agent.yaml"
    f.write_text("models:\n  providers:\n    - name: openai\n", encoding="utf-8")
    with patch(f"{_T}.resolve_config_file", return_value=f):
        assert setup_mod.has_any_provider_configured() is True


def test_has_provider_false_with_empty_providers(tmp_path):
    f = tmp_path / "echo-agent.yaml"
    f.write_text("models:\n  providers: []\n", encoding="utf-8")
    with patch(f"{_T}.resolve_config_file", return_value=f):
        assert setup_mod.has_any_provider_configured() is False
