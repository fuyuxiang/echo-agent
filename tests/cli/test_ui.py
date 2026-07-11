"""Tests for echo_agent.cli.ui — questionary wrapper with prompt.py fallback."""
from __future__ import annotations

from unittest.mock import MagicMock, patch

from echo_agent.cli import ui

_T = "echo_agent.cli.ui"


def test_use_rich_false_when_not_interactive():
    with patch(f"{_T}.is_interactive", return_value=False):
        assert ui.use_rich() is False


def test_select_fallback_uses_prompt_choice():
    # non-interactive -> falls back to prompt_choice, returns selected value
    choices = [("openai", "OpenAI", ""), ("anthropic", "Anthropic", "")]
    with patch(f"{_T}.use_rich", return_value=False), \
         patch(f"{_T}.prompt_choice", return_value=1) as pc:
        assert ui.select("provider?", choices, default="openai") == "anthropic"
    pc.assert_called_once()


def test_select_grouped_fallback_flattens_and_returns_value():
    groups = [
        ("mainstream", [("openai", "OpenAI", ""), ("anthropic", "Anthropic", "")]),
        ("local", [("ollama", "Ollama", "")]),
    ]
    with patch(f"{_T}.use_rich", return_value=False), \
         patch(f"{_T}.prompt_choice", return_value=2) as pc:
        assert ui.select_grouped("provider?", groups) == "ollama"
    pc.assert_called_once()


def test_multiselect_fallback_uses_checklist():
    choices = [("web", "Web", ""), ("tts", "TTS", ""), ("mcp", "MCP", "")]
    with patch(f"{_T}.use_rich", return_value=False), \
         patch(f"{_T}.prompt_checklist", return_value=[0, 2]) as cl:
        assert ui.multiselect("tools?", choices) == ["web", "mcp"]
    cl.assert_called_once()


def test_confirm_fallback_uses_prompt_yes_no():
    with patch(f"{_T}.use_rich", return_value=False), \
         patch(f"{_T}.prompt_yes_no", return_value=True):
        assert ui.confirm("ok?", default=False) is True


def test_password_fallback_uses_prompt_password():
    with patch(f"{_T}.use_rich", return_value=False), \
         patch(f"{_T}.prompt", return_value="secret") as p:
        assert ui.password("key?") == "secret"
    assert p.call_args.kwargs.get("password") is True


def test_password_rich_strips_surrounding_whitespace():
    # rich (questionary) backend must strip pasted keys just like the fallback.
    fake_q = MagicMock()
    fake_q.password.return_value.ask.return_value = "  sk-abc123\n"
    with patch(f"{_T}._q", fake_q), \
         patch(f"{_T}._HAS_Q", True), \
         patch(f"{_T}.is_interactive", return_value=True):
        assert ui.use_rich() is True
        assert ui.password("key?") == "sk-abc123"
    fake_q.password.assert_called_once_with("key?")


def test_note_does_not_crash():
    ui.note("hello", "success")
    ui.note("warn", "warning")


def test_spinner_is_contextmanager():
    with ui.spinner("loading"):
        pass
