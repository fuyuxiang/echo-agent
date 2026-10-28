"""Tests for the ``echo-agent setup security`` wizard section.

Regression guard for the silent gateway downgrade: setup must always write an
explicit ``security.profile`` so ``echo-agent gateway`` never implicitly tightens
to public_gateway and strips high-risk tools behind the user's back.
"""

from __future__ import annotations

from unittest.mock import patch

import pytest

from echo_agent.cli.i18n import get_locale, set_locale
from echo_agent.cli.setup import SECTION_ALIASES, SETUP_SECTIONS
from echo_agent.cli.setup import setup_security as run_setup_security

set_locale("en")

_TARGET = "echo_agent.cli.setup"


@pytest.fixture(autouse=True)
def _restore_locale():
    """Restore the process-global locale so this file never leaks a non-English
    locale into other locale-sensitive tests."""
    saved = get_locale()
    try:
        yield
    finally:
        set_locale(saved)


def _patch_prompts(choices: dict[str, int]):
    """Return ui.* replacements.

    ``setup_security`` drives its one choice through ``ui.select`` (via the
    module-level ``_choice`` helper), which returns the *value* string; the
    helper maps that back to an int index. So the fake ``ui.select`` returns
    ``str(index)`` for a matched needle, else the passed-in default value.
    """

    def _text(message, default=""):
        return default

    def _confirm(message, default=True):
        return default

    def _select(message, choices_list, default=""):
        for needle, value in choices.items():
            if needle in message:
                return str(value)
        return default

    return _text, _confirm, _select


def test_security_registered_in_section_registry():
    keys = [k for k, _ in SETUP_SECTIONS]
    assert "security" in keys
    # Ordered right after gateway so the deployment question has gateway context.
    assert keys.index("security") == keys.index("gateway") + 1
    assert SECTION_ALIASES.get("security") == "security"
    assert SECTION_ALIASES.get("profile") == "security"


def test_no_gateway_defaults_to_personal_cli():
    config: dict = {"gateway": {"enabled": False}}
    txt, cf, sel = _patch_prompts(choices={})
    with patch(f"{_TARGET}.ui.text", txt), \
         patch(f"{_TARGET}.ui.confirm", cf), \
         patch(f"{_TARGET}.ui.select", sel):
        run_setup_security(config)
    assert config["security"]["profile"] == "personal_cli"


def test_gateway_personal_choice_writes_personal_cli():
    config: dict = {"gateway": {"enabled": True}}
    # Choice index 0 == personal_cli (the default highlight).
    txt, cf, sel = _patch_prompts(choices={"trust level": 0})
    with patch(f"{_TARGET}.ui.text", txt), \
         patch(f"{_TARGET}.ui.confirm", cf), \
         patch(f"{_TARGET}.ui.select", sel):
        run_setup_security(config)
    assert config["security"]["profile"] == "personal_cli"


def test_gateway_public_choice_writes_public_gateway():
    config: dict = {"gateway": {"enabled": True}}
    txt, cf, sel = _patch_prompts(choices={"trust level": 1})
    with patch(f"{_TARGET}.ui.text", txt), \
         patch(f"{_TARGET}.ui.confirm", cf), \
         patch(f"{_TARGET}.ui.select", sel):
        run_setup_security(config)
    assert config["security"]["profile"] == "public_gateway"


def test_profile_key_always_written():
    """The core invariant: whatever the path, security.profile ends up explicit."""
    for gw_enabled, choice in [(False, {}), (True, {"trust level": 0}), (True, {"trust level": 1})]:
        config: dict = {"gateway": {"enabled": gw_enabled}}
        txt, cf, sel = _patch_prompts(choices=choice)
        with patch(f"{_TARGET}.ui.text", txt), \
             patch(f"{_TARGET}.ui.confirm", cf), \
             patch(f"{_TARGET}.ui.select", sel):
            run_setup_security(config)
        assert config.get("security", {}).get("profile") in ("personal_cli", "public_gateway")
