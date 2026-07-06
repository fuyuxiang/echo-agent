"""Tests for the ``echo-agent setup security`` wizard section.

Regression guard for the silent gateway downgrade: setup must always write an
explicit ``security.profile`` so ``echo-agent gateway`` never implicitly tightens
to public_gateway and strips high-risk tools behind the user's back.
"""

from __future__ import annotations

from unittest.mock import patch

from echo_agent.cli.i18n import set_locale
from echo_agent.cli.setup import SECTION_ALIASES, SETUP_SECTIONS
from echo_agent.cli.setup import setup_security as run_setup_security

set_locale("en")

_TARGET = "echo_agent.cli.setup"


def _patch_prompts(choices: dict[str, int]):
    def _prompt(question, default="", password=False):
        return default

    def _yn(question, default=True):
        return default

    def _choice(question, choices_list, default=0):
        for needle, value in choices.items():
            if needle in question:
                return value
        return default

    return _prompt, _yn, _choice


def test_security_registered_in_section_registry():
    keys = [k for k, _ in SETUP_SECTIONS]
    assert "security" in keys
    # Ordered right after gateway so the deployment question has gateway context.
    assert keys.index("security") == keys.index("gateway") + 1
    assert SECTION_ALIASES.get("security") == "security"
    assert SECTION_ALIASES.get("profile") == "security"


def test_no_gateway_defaults_to_personal_cli():
    config: dict = {"gateway": {"enabled": False}}
    p, yn, ch = _patch_prompts(choices={})
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_security(config)
    assert config["security"]["profile"] == "personal_cli"


def test_gateway_personal_choice_writes_personal_cli():
    config: dict = {"gateway": {"enabled": True}}
    # Choice index 0 == personal_cli (the default highlight).
    p, yn, ch = _patch_prompts(choices={"trust level": 0})
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_security(config)
    assert config["security"]["profile"] == "personal_cli"


def test_gateway_public_choice_writes_public_gateway():
    config: dict = {"gateway": {"enabled": True}}
    p, yn, ch = _patch_prompts(choices={"trust level": 1})
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_security(config)
    assert config["security"]["profile"] == "public_gateway"


def test_profile_key_always_written():
    """The core invariant: whatever the path, security.profile ends up explicit."""
    for gw_enabled, choice in [(False, {}), (True, {"trust level": 0}), (True, {"trust level": 1})]:
        config: dict = {"gateway": {"enabled": gw_enabled}}
        p, yn, ch = _patch_prompts(choices=choice)
        with patch(f"{_TARGET}.prompt", p), \
             patch(f"{_TARGET}.prompt_yes_no", yn), \
             patch(f"{_TARGET}.prompt_choice", ch):
            run_setup_security(config)
        assert config.get("security", {}).get("profile") in ("personal_cli", "public_gateway")
