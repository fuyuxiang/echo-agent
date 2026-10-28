"""Orchestration tests: quickstart pins its security boundary before saving."""
from __future__ import annotations

from unittest.mock import patch

from echo_agent.cli import setup as setup_mod
from echo_agent.cli.i18n import set_locale

set_locale("en")
_S = "echo_agent.cli.setup"


def test_quickstart_runs_language_model_permissions_and_security(tmp_path):
    calls = []
    for name in ("setup_language", "setup_permissions", "setup_terminal",
                 "setup_agent", "setup_tools", "setup_channels"):
        pass

    def _rec(fn_name):
        def _f(cfg):
            calls.append(fn_name)
            cfg.setdefault("models", {"providers": [{"name": "openai"}], "defaultModel": "gpt-4o"})
        return _f

    # _offer_gateway_start is stubbed because it probes the live service manager:
    # unstubbed it shells out to launchctl/systemctl and, on a host whose gateway
    # is enabled, would sit on a confirm prompt. Its own behaviour is covered by
    # tests/cli/test_setup_gateway_start.py.
    with patch(f"{_S}.is_interactive", return_value=True), \
         patch(f"{_S}.ui.select", return_value="quickstart"), \
         patch(f"{_S}.setup_language", _rec("language")), \
         patch(f"{_S}.setup_model", _rec("model")), \
         patch(f"{_S}.setup_permissions", _rec("permissions")), \
         patch(f"{_S}.setup_security", _rec("security")), \
         patch(f"{_S}.setup_channels", _rec("channels")), \
         patch(f"{_S}.save_config", return_value=tmp_path / "echo-agent.yaml"), \
         patch(f"{_S}._ensure_credential_key"), \
         patch(f"{_S}._offer_gateway_start"), \
         patch(f"{_S}.setup_doctor"):
        setup_mod.run_setup_wizard(config_path=str(tmp_path / "echo-agent.yaml"))

    assert calls == ["language", "model", "permissions", "security"]
    assert "channels" not in calls


def test_summary_lists_enable_commands_for_missing(capsys):
    cfg = {"models": {"providers": [{"name": "openai"}], "defaultModel": "gpt-4o"},
           "gateway": {"enabled": False}}
    setup_mod._print_summary(cfg, __import__("pathlib").Path("/tmp/x.yaml"))
    out = capsys.readouterr().out
    assert "echo-agent setup" in out
    assert "echo-agent status" in out
    assert "run the agent in the foreground" in out


def test_quickstart_persists_explicit_personal_security_profile(tmp_path):
    saved = {}

    def _save(cfg, target):
        saved.update(cfg)
        return target

    target = tmp_path / "echo-agent.yaml"
    with patch(f"{_S}.is_interactive", return_value=True), \
         patch(f"{_S}.setup_language"), \
         patch(f"{_S}.setup_model"), \
         patch(f"{_S}.setup_permissions"), \
         patch(f"{_S}.save_config", side_effect=_save), \
         patch(f"{_S}._ensure_credential_key"), \
         patch(f"{_S}._offer_gateway_start"), \
         patch(f"{_S}.setup_doctor"), \
         patch(f"{_S}._print_summary"):
        setup_mod.run_setup_wizard(config_path=str(target), flow="quickstart")

    assert saved["security"]["profile"] == "personal_cli"


def test_direct_gateway_section_reconfirms_security_before_save(tmp_path):
    calls = []
    target = tmp_path / "echo-agent.yaml"
    sections = [
        (key, (lambda cfg: calls.append("gateway")) if key == "gateway" else func)
        for key, func in setup_mod.SETUP_SECTIONS
    ]

    with patch(f"{_S}.is_interactive", return_value=True), \
         patch(f"{_S}.SETUP_SECTIONS", sections), \
         patch(f"{_S}.setup_security", side_effect=lambda cfg: calls.append("security")), \
         patch(f"{_S}.save_config", return_value=target), \
         patch(f"{_S}._offer_gateway_start"):
        setup_mod.run_setup_wizard(config_path=str(target), section="gateway")

    assert calls == ["gateway", "security"]
