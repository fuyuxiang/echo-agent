"""Behavioural tests for the real-switch fixes in channels / tools / doctor.

Covers the three regressions fixed together:
  1. un-checking a channel writes enabled=False (was silently left true);
  2. un-checking a tool (image_gen/tts/mcp/skills) writes a real enabled=False
     (were "fake" toggles that only acted on selection);
  3. doctor renders structured health probes rather than hard-coded "OK".
"""
from __future__ import annotations

from unittest.mock import patch

from echo_agent.cli import health
from echo_agent.cli import setup as setup_mod
from echo_agent.cli.i18n import set_locale

set_locale("en")
_S = "echo_agent.cli.setup"


def _noop_prompts():
    """Patch every interactive prompt used by setup_tools/setup_channels so
    the section runs headless. multiselect is patched per-test."""
    return [
        patch(f"{_S}.ui.password", return_value=""),
        patch(f"{_S}.ui.text", side_effect=lambda *a, **k: k.get("default", "")),
        patch(f"{_S}._choice", return_value=0),
        patch(f"{_S}.ui.confirm", return_value=False),
    ]


# ── Task 1: channel de-selection persists enabled=False ───────────────────────

def test_channel_deselect_writes_false():
    # telegram was enabled; user unchecks everything.
    cfg = {"channels": {"telegram": {"enabled": True, "token": "keep-me"}}}
    with patch(f"{_S}.ui.multiselect", return_value=[]):
        setup_mod.setup_channels(cfg)
    assert cfg["channels"]["telegram"]["enabled"] is False
    # credentials are preserved, only the switch flips.
    assert cfg["channels"]["telegram"]["token"] == "keep-me"


def test_channel_all_candidates_written_false_on_empty():
    cfg = {"channels": {}}
    with patch(f"{_S}.ui.multiselect", return_value=[]):
        setup_mod.setup_channels(cfg)
    # every known channel now has an explicit enabled=False.
    for ch_key, _label, _fields in setup_mod.CHANNEL_DEFS:
        assert cfg["channels"][ch_key]["enabled"] is False


def test_channel_unselected_flipped_when_other_selected():
    # discord stays enabled via selection, telegram gets disabled.
    cfg = {"channels": {"telegram": {"enabled": True}, "discord": {"enabled": True}}}
    discord_idx = next(i for i, c in enumerate(setup_mod.CHANNEL_DEFS) if c[0] == "discord")
    patches = _noop_prompts()
    for p in patches:
        p.start()
    try:
        with patch(f"{_S}.ui.multiselect", return_value=[str(discord_idx)]):
            setup_mod.setup_channels(cfg)
    finally:
        for p in patches:
            p.stop()
    assert cfg["channels"]["telegram"]["enabled"] is False
    assert cfg["channels"]["discord"]["enabled"] is True


# ── Task 2: tool de-selection persists real enabled=False ─────────────────────

def _run_tools_with_selection(cfg, selected_keys):
    idxs = [str(setup_mod.TOOL_OPTIONS.index(k)) for k in selected_keys]
    patches = _noop_prompts()
    for p in patches:
        p.start()
    try:
        with patch(f"{_S}.ui.multiselect", return_value=idxs):
            setup_mod.setup_tools(cfg)
    finally:
        for p in patches:
            p.stop()


def test_tools_deselect_flips_enabled_false_and_keeps_credentials():
    cfg = {
        "tools": {
            "image_gen": {"enabled": True, "api_key": "img-key", "backend": "openai"},
            "tts": {"enabled": True, "openai_api_key": "tts-key", "default_backend": "openai"},
            "mcp": {"enabled": True},
            "mcp_servers": {"srv": {"command": "x"}},
        },
        "skills": {"enabled": True, "skills_dir": "skills"},
    }
    # select nothing -> all optional tools disabled.
    _run_tools_with_selection(cfg, [])

    assert cfg["tools"]["image_gen"]["enabled"] is False
    assert cfg["tools"]["tts"]["enabled"] is False
    assert cfg["tools"]["mcp"]["enabled"] is False
    assert cfg["skills"]["enabled"] is False
    # credentials preserved for a later re-enable.
    assert cfg["tools"]["image_gen"]["api_key"] == "img-key"
    assert cfg["tools"]["tts"]["openai_api_key"] == "tts-key"


def test_tools_select_writes_enabled_true():
    cfg = {"tools": {}, "skills": {}}
    _run_tools_with_selection(cfg, ["mcp", "skills"])
    assert cfg["tools"]["mcp"]["enabled"] is True
    assert cfg["skills"]["enabled"] is True


# ── Task 3: doctor / health probes are real and structured ────────────────────

def test_health_returns_structured_results():
    results = health.run_health_checks({
        "models": {"providers": [{"name": "openai", "api_key": "sk"}]},
        "gateway": {"enabled": False},
        "workspace": "/tmp/echo-doctor-test",
    })
    assert isinstance(results, list) and results
    for item in results:
        assert set(item) >= {"name", "status", "detail"}
        assert item["status"] in (health.OK, health.WARN, health.FAIL)
    # provider with a key present is OK.
    prov = next(r for r in results if "openai" in r["name"])
    assert prov["status"] == health.OK


def test_health_provider_missing_key_warns():
    results = health.check_providers({"models": {"providers": [{"name": "openai"}]}})
    assert results[0]["status"] == health.WARN


def test_health_gateway_port_zero_warns():
    r = health.check_gateway({"gateway": {"enabled": True, "port": 0}})
    assert r["status"] == health.WARN


def test_health_gateway_disabled_warns():
    r = health.check_gateway({"gateway": {"enabled": False}})
    assert r["status"] == health.WARN


def test_doctor_renders_probes(capsys):
    cfg = {"models": {"providers": [{"name": "openai", "api_key": "sk"}]},
           "workspace": "/tmp/echo-doctor-test"}
    with patch(f"{_S}._print_section_header"):
        setup_mod.setup_doctor(cfg)
    out = capsys.readouterr().out
    assert "provider" in out.lower()
    assert "workspace" in out.lower()
