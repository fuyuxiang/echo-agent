"""Orchestration tests: quickstart路径只跑 3 步 + 总结页给出下一步命令。"""
from __future__ import annotations

from unittest.mock import patch

from echo_agent.cli import setup as setup_mod
from echo_agent.cli.i18n import set_locale

set_locale("en")
_S = "echo_agent.cli.setup"


def test_quickstart_runs_language_model_permissions_only(tmp_path):
    calls = []
    for name in ("setup_language", "setup_permissions", "setup_terminal",
                 "setup_agent", "setup_tools", "setup_channels"):
        pass

    def _rec(fn_name):
        def _f(cfg):
            calls.append(fn_name)
            cfg.setdefault("models", {"providers": [{"name": "openai"}], "defaultModel": "gpt-4o"})
        return _f

    with patch(f"{_S}.is_interactive", return_value=True), \
         patch(f"{_S}.ui.select", return_value="quickstart"), \
         patch(f"{_S}.setup_language", _rec("language")), \
         patch(f"{_S}.setup_model", _rec("model")), \
         patch(f"{_S}.setup_permissions", _rec("permissions")), \
         patch(f"{_S}.setup_channels", _rec("channels")), \
         patch(f"{_S}.save_config", return_value=tmp_path / "echo-agent.yaml"), \
         patch(f"{_S}._ensure_credential_key"), \
         patch(f"{_S}.setup_doctor"):
        setup_mod.run_setup_wizard(config_path=str(tmp_path / "echo-agent.yaml"))

    assert calls == ["language", "model", "permissions"]
    assert "channels" not in calls


def test_summary_lists_enable_commands_for_missing(capsys):
    cfg = {"models": {"providers": [{"name": "openai"}], "defaultModel": "gpt-4o"},
           "gateway": {"enabled": False}}
    setup_mod._print_summary(cfg, __import__("pathlib").Path("/tmp/x.yaml"))
    out = capsys.readouterr().out
    assert "echo-agent setup" in out
    assert "echo-agent status" in out
    assert "start the agent" in out
