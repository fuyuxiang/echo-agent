"""Tests for the ``echo-agent setup evolution`` wizard section."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

from echo_agent.cli.i18n import set_locale
from echo_agent.cli.setup import (
    SECTION_ALIASES,
    SETUP_SECTIONS,
)
from echo_agent.cli.setup import setup_evolution as run_setup_evolution

# Pin locale to English so prompt-text matching is deterministic regardless
# of the developer's machine.
set_locale("en")

# Path to the prompt helpers we need to patch. We patch on the module that
# defines ``setup_evolution`` so the patches are applied to its closure.
_TARGET = "echo_agent.cli.setup"


def _patch_prompts(answers: dict[str, str], yes_no: dict[str, bool], choices: dict[str, int]):
    """Build the (prompt, prompt_yes_no, prompt_choice) replacements.

    Each lookup falls back to a passthrough default — anything we forget to
    patch raises ``KeyError`` so missing assumptions surface immediately.
    """

    seen_prompt: list[tuple[str, str]] = []
    seen_yn: list[tuple[str, bool]] = []
    seen_choice: list[tuple[str, int]] = []

    def _prompt(question, default="", password=False):
        seen_prompt.append((question, default))
        # Match by suffix, so "  Trajectory threshold" matches "threshold".
        for needle, value in answers.items():
            if needle in question:
                return value
        return default

    def _yn(question, default=True):
        seen_yn.append((question, default))
        for needle, value in yes_no.items():
            if needle in question:
                return value
        return default

    def _choice(question, choices_list, default=0):
        seen_choice.append((question, default))
        for needle, value in choices.items():
            if needle in question:
                return value
        return default

    return _prompt, _yn, _choice, seen_prompt, seen_yn, seen_choice


def test_evolution_registered_in_section_registry():
    keys = [k for k, _ in SETUP_SECTIONS]
    assert keys[-1] == "evolution"
    assert SECTION_ALIASES.get("evolution") == "evolution"
    assert SECTION_ALIASES.get("evolve") == "evolution"
    assert SECTION_ALIASES.get("self-evolve") == "evolution"


def test_setup_evolution_disabled_path_keeps_record_default(tmp_path: Path):
    """Declining the master switch must not write any other knob."""
    config: dict = {"workspace": str(tmp_path)}
    p, yn, ch, *_ = _patch_prompts(
        answers={},
        yes_no={"Enable self-evolution": False},
        choices={},
    )
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_evolution(config)

    assert config["evolution"]["enabled"] is False
    # When disabled we still set record_trajectories to its default-true so the
    # next "enable" run does not surprise the user.
    assert config["evolution"]["record_trajectories"] is True
    # No other operational knobs were written.
    assert "trigger_mode" not in config["evolution"]
    assert "regression_threshold" not in config["evolution"]


def test_setup_evolution_threshold_path(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    p, yn, ch, *_ = _patch_prompts(
        answers={
            "Trajectory threshold": "25",
            "Baseline eval dataset path": "data/eval/custom.yaml",
            "Regression threshold": "0.10",
            "Max candidates per run": "2",
            "Trajectory retention": "14",
            "Eval parallelism": "1",
            "Eval per-case timeout": "30",
        },
        yes_no={
            "Enable self-evolution": True,
            "Require strict improvement": True,
            "Hold even passing candidates": False,
            "Redact tool arguments": True,
            "Record trajectories": True,
        },
        choices={
            "Trigger mode": 1,  # threshold
        },
    )
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_evolution(config)

    evo = config["evolution"]
    assert evo["enabled"] is True
    assert evo["trigger_mode"] == "threshold"
    assert evo["threshold_trajectories"] == 25
    assert "cron_expression" not in evo
    assert evo["eval_dataset_path"] == "data/eval/custom.yaml"
    assert evo["require_strict_improvement"] is True
    assert evo["regression_threshold"] == pytest.approx(0.10)
    assert evo["candidate_review_required"] is False
    assert evo["max_candidates_per_run"] == 2
    assert evo["trajectory_retention_days"] == 14
    assert evo["redact_args"] is True
    assert evo["record_trajectories"] is True
    assert evo["eval_parallel"] == 1
    assert evo["eval_timeout_seconds"] == 30

    # Baseline dataset should have been seeded under the workspace.
    seeded = tmp_path / "data/eval/custom.yaml"
    assert seeded.exists()
    body = seeded.read_text(encoding="utf-8")
    assert "cases:" in body


def test_setup_evolution_scheduled_path(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    p, yn, ch, *_ = _patch_prompts(
        answers={
            "Cron expression": "*/30 * * * *",
            "Baseline eval dataset path": "data/eval/baseline.yaml",
            "Regression threshold": "0.05",
            "Max candidates per run": "3",
            "Trajectory retention": "30",
            "Eval parallelism": "2",
            "Eval per-case timeout": "60",
        },
        yes_no={
            "Enable self-evolution": True,
            "Require strict improvement": True,
            "Hold even passing candidates": False,
            "Redact tool arguments": True,
            "Record trajectories": True,
        },
        choices={"Trigger mode": 2},  # scheduled
    )
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_evolution(config)

    evo = config["evolution"]
    assert evo["trigger_mode"] == "scheduled"
    assert evo["cron_expression"] == "*/30 * * * *"
    assert "threshold_trajectories" not in evo


def test_setup_evolution_invalid_regression_keeps_old_value(tmp_path: Path):
    """Out-of-range regression threshold must be rejected; default preserved."""
    config: dict = {
        "workspace": str(tmp_path),
        "evolution": {"regression_threshold": 0.05},
    }
    p, yn, ch, *_ = _patch_prompts(
        answers={
            "Trajectory threshold": "50",
            "Baseline eval dataset path": "data/eval/baseline.yaml",
            "Regression threshold": "0.99",  # rejected: > 0.5
            "Max candidates per run": "3",
            "Trajectory retention": "30",
            "Eval parallelism": "2",
            "Eval per-case timeout": "60",
        },
        yes_no={
            "Enable self-evolution": True,
            "Require strict improvement": True,
            "Hold even passing candidates": False,
            "Redact tool arguments": True,
            "Record trajectories": True,
        },
        choices={"Trigger mode": 1},
    )
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_evolution(config)

    # Old value is preserved when the new value is invalid.
    assert config["evolution"]["regression_threshold"] == pytest.approx(0.05)


def test_setup_evolution_does_not_overwrite_existing_dataset(tmp_path: Path):
    config: dict = {"workspace": str(tmp_path)}
    seeded = tmp_path / "data/eval/baseline.yaml"
    seeded.parent.mkdir(parents=True, exist_ok=True)
    seeded.write_text("cases: [my-existing-case]\n", encoding="utf-8")

    p, yn, ch, *_ = _patch_prompts(
        answers={
            "Trajectory threshold": "50",
            "Baseline eval dataset path": "data/eval/baseline.yaml",
            "Regression threshold": "0.05",
            "Max candidates per run": "3",
            "Trajectory retention": "30",
            "Eval parallelism": "2",
            "Eval per-case timeout": "60",
        },
        yes_no={
            "Enable self-evolution": True,
            "Require strict improvement": True,
            "Hold even passing candidates": False,
            "Redact tool arguments": True,
            "Record trajectories": True,
        },
        choices={"Trigger mode": 1},
    )
    with patch(f"{_TARGET}.prompt", p), \
         patch(f"{_TARGET}.prompt_yes_no", yn), \
         patch(f"{_TARGET}.prompt_choice", ch):
        run_setup_evolution(config)

    # The pre-existing dataset must be untouched.
    assert seeded.read_text(encoding="utf-8") == "cases: [my-existing-case]\n"
