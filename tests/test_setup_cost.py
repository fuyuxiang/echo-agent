"""Tests for the ``echo-agent setup cost`` wizard section."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

from echo_agent.cli.i18n import set_locale
from echo_agent.cli.setup import SECTION_ALIASES, SETUP_SECTIONS
from echo_agent.cli.setup import setup_cost as run_setup_cost

set_locale("en")

_TARGET = "echo_agent.cli.setup"


def _patch_prompts(answers: dict[str, str], yes_no: dict[str, bool]):
    def _prompt(question, default="", password=False):
        for needle, value in answers.items():
            if needle in question:
                return value
        return default

    def _yn(question, default=True):
        for needle, value in yes_no.items():
            if needle in question:
                return value
        return default

    return _prompt, _yn


def test_cost_registered_in_section_registry():
    keys = [k for k, _ in SETUP_SECTIONS]
    assert keys[-1] == "cost"
    assert SECTION_ALIASES.get("cost") == "cost"
    assert SECTION_ALIASES.get("budget") == "cost"


def test_setup_cost_disabled_path(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    p, yn = _patch_prompts(answers={}, yes_no={"Enable cost budget": False})
    with patch(f"{_TARGET}.prompt", p), patch(f"{_TARGET}.prompt_yes_no", yn):
        run_setup_cost(config)
    assert config["cost"]["enabled"] is False
    assert "daily_budget_usd" not in config["cost"]


def test_setup_cost_enabled_positive(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    p, yn = _patch_prompts(
        answers={"Daily budget cap": "5.0"},
        yes_no={"Enable cost budget": True},
    )
    with patch(f"{_TARGET}.prompt", p), patch(f"{_TARGET}.prompt_yes_no", yn):
        run_setup_cost(config)
    assert config["cost"]["enabled"] is True
    assert config["cost"]["daily_budget_usd"] == 5.0


def test_setup_cost_enabled_zero(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    p, yn = _patch_prompts(
        answers={"Daily budget cap": "0"},
        yes_no={"Enable cost budget": True},
    )
    with patch(f"{_TARGET}.prompt", p), patch(f"{_TARGET}.prompt_yes_no", yn):
        run_setup_cost(config)
    assert config["cost"]["enabled"] is True
    assert config["cost"]["daily_budget_usd"] == 0.0


def test_setup_cost_enabled_invalid_falls_back(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path), "cost": {"daily_budget_usd": 3.5}}
    p, yn = _patch_prompts(
        answers={"Daily budget cap": "abc"},
        yes_no={"Enable cost budget": True},
    )
    with patch(f"{_TARGET}.prompt", p), patch(f"{_TARGET}.prompt_yes_no", yn):
        run_setup_cost(config)
    assert config["cost"]["enabled"] is True
    # Invalid input must preserve the prior budget, not silently zero it.
    assert config["cost"]["daily_budget_usd"] == 3.5


def test_setup_cost_enabled_negative_clamped_to_zero(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    p, yn = _patch_prompts(
        answers={"Daily budget cap": "-2"},
        yes_no={"Enable cost budget": True},
    )
    with patch(f"{_TARGET}.prompt", p), patch(f"{_TARGET}.prompt_yes_no", yn):
        run_setup_cost(config)
    assert config["cost"]["enabled"] is True
    assert config["cost"]["daily_budget_usd"] == 0.0


def test_setup_cost_enabled_nonfinite_falls_back(tmp_path: Path):
    for bad in ("nan", "inf"):
        config: dict = {"workspace": str(tmp_path), "cost": {"daily_budget_usd": 4.0}}
        p, yn = _patch_prompts(
            answers={"Daily budget cap": bad},
            yes_no={"Enable cost budget": True},
        )
        with patch(f"{_TARGET}.prompt", p), patch(f"{_TARGET}.prompt_yes_no", yn):
            run_setup_cost(config)
        # Non-finite input is rejected and the prior finite budget is kept.
        assert config["cost"]["daily_budget_usd"] == 4.0
