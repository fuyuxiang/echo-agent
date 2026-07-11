"""Behavioural tests for the redesigned model section."""
from __future__ import annotations

from unittest.mock import patch

from echo_agent.cli import setup as setup_mod
from echo_agent.cli.i18n import set_locale
from echo_agent.cli.setup import model_verify as mv

set_locale("en")
_S = "echo_agent.cli.setup"


def test_model_section_deepseek_prefills_dialect_and_base():
    cfg = {}
    with patch(f"{_S}.ui.select_grouped", return_value="deepseek"), \
         patch(f"{_S}.ui.password", return_value="sk-ds"), \
         patch(f"{_S}.ui.select", return_value="deepseek-chat"), \
         patch(f"{_S}.list_models", return_value=[]), \
         patch(f"{_S}.verify_model", return_value=mv.VerifyResult("ok")):
        setup_mod.setup_model(cfg)
    prov = cfg["models"]["providers"][0]
    assert prov["name"] == "openai"                     # dialect
    assert prov["apiBase"] == "https://api.deepseek.com/v1"
    assert prov["apiKey"] == "sk-ds"
    assert cfg["models"]["defaultModel"] == "deepseek-chat"


def test_model_section_uses_dynamic_list_when_available():
    cfg = {}
    with patch(f"{_S}.ui.select_grouped", return_value="openai"), \
         patch(f"{_S}.ui.password", return_value="sk-x"), \
         patch(f"{_S}.list_models", return_value=["gpt-4o", "o3"]), \
         patch(f"{_S}.ui.select", return_value="o3") as sel, \
         patch(f"{_S}.verify_model", return_value=mv.VerifyResult("ok")):
        setup_mod.setup_model(cfg)
    # dynamic models were offered to ui.select
    offered = [c[0] for c in sel.call_args.args[1]]
    assert "o3" in offered
    assert cfg["models"]["defaultModel"] == "o3"


def test_model_section_verify_error_can_skip():
    cfg = {}
    # verify returns error; user picks "skip" -> config still saved
    with patch(f"{_S}.ui.select_grouped", return_value="openai"), \
         patch(f"{_S}.ui.password", return_value="bad"), \
         patch(f"{_S}.list_models", return_value=[]), \
         patch(f"{_S}.ui.select", side_effect=["gpt-4o", "skip"]), \
         patch(f"{_S}.verify_model", return_value=mv.VerifyResult("error", "401")):
        setup_mod.setup_model(cfg)
    assert cfg["models"]["providers"][0]["apiKey"] == "bad"


def test_model_section_allowlist_includes_chosen_dynamic_model():
    # openrouter: dialect != "openai" so a models allowlist IS written. The
    # chosen model is a dynamically-listed one NOT in fallback_models; it must
    # still land in provider_entry["models"] so the router can match it.
    cfg = {}
    dynamic = "meta-llama/llama-3.3-70b-instruct"
    with patch(f"{_S}.ui.select_grouped", return_value="openrouter"), \
         patch(f"{_S}.ui.password", return_value="sk-or"), \
         patch(f"{_S}.list_models", return_value=[dynamic, "openai/gpt-4o"]), \
         patch(f"{_S}.ui.select", return_value=dynamic), \
         patch(f"{_S}.verify_model", return_value=mv.VerifyResult("ok")):
        setup_mod.setup_model(cfg)
    prov = cfg["models"]["providers"][0]
    assert prov["name"] == "openrouter"
    assert cfg["models"]["defaultModel"] == dynamic
    assert dynamic in prov["models"]           # chosen model present in allowlist
    assert "openai/gpt-4o" in prov["models"]   # fallback models retained too


def test_model_section_custom_prompts_api_base():
    cfg = {}
    with patch(f"{_S}.ui.select_grouped", return_value="custom"), \
         patch(f"{_S}.ui.text", return_value="https://my.host/v1") as txt, \
         patch(f"{_S}.ui.password", return_value="sk-c"), \
         patch(f"{_S}.list_models", return_value=[]), \
         patch(f"{_S}.ui.select", return_value="local-model"), \
         patch(f"{_S}.verify_model", return_value=mv.VerifyResult("unreachable")):
        setup_mod.setup_model(cfg)
    assert cfg["models"]["providers"][0]["apiBase"] == "https://my.host/v1"
    txt.assert_called()
