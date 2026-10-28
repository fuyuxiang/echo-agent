"""Tests for echo_agent.cli.prompt — interactive prompt helpers.

input()/getpass are mocked; we never read real stdin. Complements the
prompt_yes_no/is_interactive cases already in tests/test_cli_modules.py by
covering prompt(), prompt_choice(), prompt_checklist() and their
KeyboardInterrupt/EOF abort paths.
"""

from __future__ import annotations

from unittest.mock import patch

import pytest

from echo_agent.cli import prompt as prompt_mod

_T = "echo_agent.cli.prompt"


# ── prompt ────────────────────────────────────────────────────────────────────

def test_prompt_returns_input():
    with patch("builtins.input", return_value="  value  "):
        assert prompt_mod.prompt("Name") == "value"


def test_prompt_empty_uses_default():
    with patch("builtins.input", return_value=""):
        assert prompt_mod.prompt("Name", default="def") == "def"


def test_prompt_password_uses_getpass():
    with patch(f"{_T}.getpass.getpass", return_value="secret") as gp:
        assert prompt_mod.prompt("Token", password=True) == "secret"
    gp.assert_called_once()


def test_prompt_keyboard_interrupt_aborts():
    with patch("builtins.input", side_effect=KeyboardInterrupt):
        with pytest.raises(prompt_mod.PromptAborted):
            prompt_mod.prompt("Name")


def test_prompt_eof_aborts():
    with patch("builtins.input", side_effect=EOFError):
        with pytest.raises(prompt_mod.PromptAborted):
            prompt_mod.prompt("Name")


def test_abort_is_not_a_silent_success():
    """An abort must never look like "the work finished".

    prompt() used to sys.exit(0) here, so piping an empty stdin into any
    confirm-gated command produced exit status 0 without doing anything.
    """
    with patch("builtins.input", side_effect=EOFError):
        with pytest.raises(prompt_mod.PromptAborted):
            prompt_mod.prompt("Name")
        assert not issubclass(prompt_mod.PromptAborted, SystemExit)


# ── prompt_yes_no — invalid then valid ─────────────────────────────────────────

def test_prompt_yes_no_reprompts_on_invalid(capsys):
    with patch("builtins.input", side_effect=["maybe", "y"]):
        assert prompt_mod.prompt_yes_no("Continue?") is True
    assert "Please enter y or n" in capsys.readouterr().out


def test_prompt_yes_no_interrupt_aborts():
    with patch("builtins.input", side_effect=KeyboardInterrupt):
        with pytest.raises(prompt_mod.PromptAborted):
            prompt_mod.prompt_yes_no("Continue?")


# ── prompt_choice ──────────────────────────────────────────────────────────────

def test_prompt_choice_valid_selection():
    with patch("builtins.input", return_value="2"):
        idx = prompt_mod.prompt_choice("Pick", ["a", "b", "c"])
    assert idx == 1


def test_prompt_choice_empty_uses_default():
    with patch("builtins.input", return_value=""):
        idx = prompt_mod.prompt_choice("Pick", ["a", "b"], default=1)
    assert idx == 1


def test_prompt_choice_reprompts_out_of_range(capsys):
    with patch("builtins.input", side_effect=["9", "1"]):
        idx = prompt_mod.prompt_choice("Pick", ["a", "b"])
    assert idx == 0
    assert "between 1 and 2" in capsys.readouterr().out


def test_prompt_choice_reprompts_non_numeric(capsys):
    with patch("builtins.input", side_effect=["abc", "1"]):
        idx = prompt_mod.prompt_choice("Pick", ["a", "b"])
    assert idx == 0
    assert "Please enter a number" in capsys.readouterr().out


def test_prompt_choice_interrupt_aborts():
    with patch("builtins.input", side_effect=EOFError):
        with pytest.raises(prompt_mod.PromptAborted):
            prompt_mod.prompt_choice("Pick", ["a"])


# ── prompt_checklist ────────────────────────────────────────────────────────────

def test_prompt_checklist_toggle_then_done():
    # toggle item 1 on, then confirm.
    with patch("builtins.input", side_effect=["1", "done"]):
        result = prompt_mod.prompt_checklist("Pick", ["a", "b", "c"])
    assert result == [0]


def test_prompt_checklist_preselected_then_immediate_done():
    with patch("builtins.input", side_effect=[""]):
        result = prompt_mod.prompt_checklist("Pick", ["a", "b"], pre_selected=[1])
    assert result == [1]


def test_prompt_checklist_none_clears():
    with patch("builtins.input", side_effect=["none", "done"]):
        result = prompt_mod.prompt_checklist("Pick", ["a", "b"], pre_selected=[0, 1])
    assert result == []


def test_prompt_checklist_invalid_index_reprompts(capsys):
    with patch("builtins.input", side_effect=["9", "done"]):
        result = prompt_mod.prompt_checklist("Pick", ["a", "b"])
    assert result == []
    assert "Please enter 1-2" in capsys.readouterr().out


def test_prompt_checklist_non_numeric_reprompts(capsys):
    with patch("builtins.input", side_effect=["xyz", "done"]):
        prompt_mod.prompt_checklist("Pick", ["a"])
    assert "Enter a number" in capsys.readouterr().out


def test_prompt_checklist_interrupt_aborts():
    with patch("builtins.input", side_effect=KeyboardInterrupt):
        with pytest.raises(prompt_mod.PromptAborted):
            prompt_mod.prompt_checklist("Pick", ["a"])
