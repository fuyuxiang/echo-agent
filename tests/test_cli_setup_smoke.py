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
from echo_agent.cli.tui.brand import Brand, ECHO_LOGO_ART

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


def test_banner_mirrors_cli_brand_without_legacy_box(capsys):
    set_locale("en")
    with patch(f"{_T}.load_brand", return_value=Brand()):
        setup_mod._print_banner()
    out = capsys.readouterr().out
    assert ECHO_LOGO_ART[0] in out
    assert "· agent · setup" in out
    assert "┌" not in out
    assert "◆" not in out


def test_section_header_uses_cli_prompt_sigil(capsys):
    set_locale("en")
    setup_mod._print_section_header("model")
    out = capsys.readouterr().out
    assert "❯ Model & Provider" in out
    assert "◆" not in out


# ── _capability_check ──────────────────────────────────────────────────────────

def test_capability_check_empty_config():
    checks = setup_mod._capability_check({})
    labels = [c[0] for c in checks]
    oks = [c[1] for c in checks]
    # Always returns a fixed-length checklist; first item is the model check.
    assert len(checks) >= 10
    assert all(isinstance(label, str) and label for label in labels)
    assert oks[0] is False  # no providers


def test_capability_check_full_config(monkeypatch):
    # The gateway check now reads the live runtime state, so pin it: otherwise the
    # verdict would depend on whether a gateway happens to be listening on this
    # machine, and the probe would shell out to the real service manager.
    from echo_agent.cli.runtime_probe import GatewayRuntime, GatewayState

    monkeypatch.setattr(
        setup_mod, "probe_gateway",
        lambda **kw: GatewayRuntime(
            state=GatewayState.RUNNING, enabled=True, host="0.0.0.0",
            port=58123, listening=True,
        ),
    )
    config = {
        "models": {"providers": [{"name": "openai"}]},
        "channels": {"telegram": {"enabled": True}, "cli": {"enabled": True}},
        "gateway": {"enabled": True, "host": "0.0.0.0", "port": 58123},
        "tools": {"profile": "full", "mcp_servers": {"x": {}}},
        "observability": {"log_level": "DEBUG", "otel_enabled": True,
                          "otel_endpoint": "http://otel:4317"},
    }
    checks = setup_mod._capability_check(config)
    assert checks[0][1] is True  # model present
    # The channel check reports the enabled non-cli channel.
    channel_check = next(c for c in checks if "telegram" in c[2])
    assert channel_check[1] is True
    # A listening gateway keeps its ✓, and reports the loopback host to connect to
    # rather than the 0.0.0.0 wildcard it is bound on.
    gateway_check = next(c for c in checks if "127.0.0.1:58123" in c[0])
    assert gateway_check[1] is True


def test_capability_check_enabled_but_not_running_is_not_ok(monkeypatch):
    """The regression: doctor read only the YAML, so `gateway.enabled: true`
    printed a ✓ for a gateway nobody was serving — on the very screen users rely
    on to decide whether their setup worked."""
    from echo_agent.cli.runtime_probe import GatewayRuntime, GatewayState

    monkeypatch.setattr(
        setup_mod, "probe_gateway",
        lambda **kw: GatewayRuntime(
            state=GatewayState.NOT_INSTALLED, enabled=True,
            host="127.0.0.1", port=58123,
        ),
    )
    checks = setup_mod._capability_check({"gateway": {"enabled": True, "port": 58123}})
    gateway_check = next(c for c in checks if "ateway" in c[0])
    assert gateway_check[1] is False
    assert "echo-agent gateway start" in gateway_check[2]


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

    # The save path now ends in the startup handoff, which probes the real
    # service manager (launchctl/systemctl) unless stubbed. See
    # tests/cli/test_setup_gateway_start.py for its own coverage.
    with patch(f"{_T}.is_interactive", return_value=True), \
         patch(f"{_T}._setup_config_target", return_value=target), \
         patch(f"{_T}._load_existing_config", return_value=({}, None)), \
         patch(f"{_T}._print_banner"), \
         patch(f"{_T}._offer_gateway_start"), \
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


class TestWizardCancellation:
    """Cancelling the wizard stays a clean exit 0.

    The prompt helpers now raise PromptAborted instead of exiting 0 themselves,
    so that commands whose work never happened can report a failure. The wizard
    is the opposite case — walking away from setup is a normal outcome — so it
    absorbs the abort at its own boundary. This test pins that split so a future
    change to the prompt layer cannot silently turn `Ctrl-C during setup` into a
    traceback or a non-zero code.
    """

    def test_abort_mid_wizard_returns_zero(self, tmp_path):
        from echo_agent.cli.prompt import PromptAborted

        # Patched on ui rather than on the section function: SETUP_SECTIONS holds
        # direct function references, so replacing setup_cost on the module would
        # not affect the entry the wizard actually calls.
        with patch(f"{_T}.is_interactive", return_value=True), \
             patch(f"{_T}._setup_config_target", return_value=tmp_path / "c.yaml"), \
             patch(f"{_T}._load_existing_config", return_value=({}, None)), \
             patch(f"{_T}._print_banner"), \
             patch(f"{_T}.save_config") as save, \
             patch("echo_agent.cli.ui.confirm", side_effect=PromptAborted("EOFError")):
            rc = setup_mod.run_setup_wizard(section="cost")
        assert rc == 0
        save.assert_not_called()  # 中止即不落盘

    def test_first_run_abort_exits_zero(self, tmp_path):
        from echo_agent.cli.prompt import PromptAborted

        with patch(f"{_T}.is_interactive", return_value=True), \
             patch(f"{_T}.has_any_provider_configured", return_value=False), \
             patch(f"{_T}._setup_config_target", return_value=tmp_path / "missing.yaml"), \
             patch(f"{_T}._load_existing_config", return_value=({}, None)), \
             patch(f"{_T}.prompt_yes_no", side_effect=PromptAborted("EOFError")):
            with pytest.raises(SystemExit) as exc:
                setup_mod.prompt_first_run_setup()
        assert exc.value.code == 0


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
