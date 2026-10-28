"""Tests for CLI utilities — colors, status, prompt."""

from __future__ import annotations

from unittest.mock import patch, MagicMock

import pytest

from echo_agent.cli.colors import (
    Colors, color, print_header, print_success, print_info, print_warning, print_error,
    set_color_override,
)


@pytest.fixture
def _force_color():
    """color() now suppresses ANSI on non-TTY stdout (pytest captures to a
    pipe). These tests exercise the coloring itself, so force it on."""
    set_color_override(True)
    try:
        yield
    finally:
        set_color_override(None)


# ══════════════════════════════════════════════════════════════════════════════
# color() function
# ══════════════════════════════════════════════════════════════════════════════


class TestColorFunction:
    def test_no_codes(self):
        assert color("hello") == "hello"

    def test_single_code(self, _force_color):
        result = color("text", Colors.RED)
        assert result == f"{Colors.RED}text{Colors.RESET}"

    def test_multiple_codes(self, _force_color):
        result = color("text", Colors.BOLD, Colors.CYAN)
        assert result == f"{Colors.BOLD}{Colors.CYAN}text{Colors.RESET}"

    def test_empty_text(self, _force_color):
        result = color("", Colors.GREEN)
        assert result == f"{Colors.GREEN}{Colors.RESET}"


class TestColorGuards:
    """color() must drop ANSI on non-TTY / NO_COLOR / explicit override off."""

    def test_non_tty_returns_plain(self):
        # Ensure auto-detection is active and stdout looks like a non-TTY pipe.
        set_color_override(None)
        fake = MagicMock()
        fake.isatty.return_value = False
        with patch("echo_agent.cli.colors.sys.stdout", fake):
            assert color("hi", Colors.RED) == "hi"

    def test_no_color_env_returns_plain(self, monkeypatch):
        set_color_override(None)
        monkeypatch.setenv("NO_COLOR", "1")
        fake = MagicMock()
        fake.isatty.return_value = True  # TTY, but NO_COLOR wins
        with patch("echo_agent.cli.colors.sys.stdout", fake):
            assert color("hi", Colors.RED) == "hi"

    def test_tty_without_no_color_keeps_ansi(self, monkeypatch):
        set_color_override(None)
        monkeypatch.delenv("NO_COLOR", raising=False)
        fake = MagicMock()
        fake.isatty.return_value = True
        with patch("echo_agent.cli.colors.sys.stdout", fake):
            assert color("hi", Colors.RED) == f"{Colors.RED}hi{Colors.RESET}"

    def test_override_off_beats_tty(self, monkeypatch):
        monkeypatch.delenv("NO_COLOR", raising=False)
        fake = MagicMock()
        fake.isatty.return_value = True
        set_color_override(False)
        try:
            with patch("echo_agent.cli.colors.sys.stdout", fake):
                assert color("hi", Colors.RED) == "hi"
        finally:
            set_color_override(None)


# ══════════════════════════════════════════════════════════════════════════════
# print_* functions
# ══════════════════════════════════════════════════════════════════════════════


class TestPrintFunctions:
    def test_print_header(self, capsys):
        print_header("Test Header")
        captured = capsys.readouterr()
        assert "Test Header" in captured.out

    def test_print_success(self, capsys, _force_color):
        print_success("All good")
        captured = capsys.readouterr()
        assert "All good" in captured.out
        assert Colors.GREEN in captured.out

    def test_print_info(self, capsys):
        print_info("Some info")
        captured = capsys.readouterr()
        assert "Some info" in captured.out

    def test_print_warning(self, capsys, _force_color):
        print_warning("Watch out")
        captured = capsys.readouterr()
        assert "Watch out" in captured.out
        assert Colors.YELLOW in captured.out

    def test_print_error(self, capsys, _force_color):
        print_error("Failed")
        captured = capsys.readouterr()
        assert "Failed" in captured.out
        assert Colors.RED in captured.out


# ══════════════════════════════════════════════════════════════════════════════
# _provider_credential_status
# ══════════════════════════════════════════════════════════════════════════════


class TestProviderCredentialStatus:
    def _make_provider_config(self, name="openai", api_key="", credential_pool=None):
        cfg = MagicMock()
        cfg.name = name
        cfg.api_key = api_key
        cfg.credential_pool = credential_pool
        return cfg

    def test_credential_pool_configured(self):
        from echo_agent.cli.status import _provider_credential_status
        cfg = self._make_provider_config(credential_pool=["key1", "key2"])
        text, clr = _provider_credential_status(cfg)
        assert "credential pool" in text.lower()
        assert clr == Colors.GREEN

    def test_api_key_configured(self):
        from echo_agent.cli.status import _provider_credential_status
        cfg = self._make_provider_config(api_key="sk-xxx")
        text, clr = _provider_credential_status(cfg)
        assert "api key configured" in text.lower()
        assert clr == Colors.GREEN

    def test_bedrock_uses_aws_env(self):
        from echo_agent.cli.status import _provider_credential_status
        cfg = self._make_provider_config(name="bedrock")
        text, clr = _provider_credential_status(cfg)
        assert "aws" in text.lower()
        assert clr == Colors.CYAN

    def test_missing_api_key(self):
        from echo_agent.cli.status import _provider_credential_status
        cfg = self._make_provider_config(name="openai")
        text, clr = _provider_credential_status(cfg)
        assert "missing" in text.lower()
        assert clr == Colors.YELLOW


# ══════════════════════════════════════════════════════════════════════════════
# prompt.py — is_interactive, prompt_yes_no
# ══════════════════════════════════════════════════════════════════════════════


class TestIsInteractive:
    def test_is_interactive_tty(self):
        from echo_agent.cli.prompt import is_interactive
        with patch("sys.stdin") as mock_stdin:
            mock_stdin.isatty = MagicMock(return_value=True)
            assert is_interactive() is True

    def test_is_interactive_not_tty(self):
        from echo_agent.cli.prompt import is_interactive
        with patch("sys.stdin") as mock_stdin:
            mock_stdin.isatty = MagicMock(return_value=False)
            assert is_interactive() is False


class TestPromptYesNo:
    def test_yes_input(self):
        from echo_agent.cli.prompt import prompt_yes_no
        with patch("builtins.input", return_value="y"):
            assert prompt_yes_no("Continue?") is True

    def test_no_input(self):
        from echo_agent.cli.prompt import prompt_yes_no
        with patch("builtins.input", return_value="n"):
            assert prompt_yes_no("Continue?") is False

    def test_empty_input_default_true(self):
        from echo_agent.cli.prompt import prompt_yes_no
        with patch("builtins.input", return_value=""):
            assert prompt_yes_no("Continue?", default=True) is True

    def test_empty_input_default_false(self):
        from echo_agent.cli.prompt import prompt_yes_no
        with patch("builtins.input", return_value=""):
            assert prompt_yes_no("Continue?", default=False) is False

    def test_yes_full_word(self):
        from echo_agent.cli.prompt import prompt_yes_no
        with patch("builtins.input", return_value="yes"):
            assert prompt_yes_no("Continue?") is True

    def test_no_full_word(self):
        from echo_agent.cli.prompt import prompt_yes_no
        with patch("builtins.input", return_value="no"):
            assert prompt_yes_no("Continue?") is False


def test_setup_ensures_credential_key(tmp_path, monkeypatch):
    """setup finalize 应在工作区生成 .credential_key（env 未设时）。"""
    monkeypatch.delenv("ECHO_AGENT_CREDENTIAL_KEY", raising=False)
    from echo_agent.cli.setup import _ensure_credential_key

    _ensure_credential_key(tmp_path)
    key_file = tmp_path / ".credential_key"
    assert key_file.exists()
    import stat as _stat
    assert _stat.S_IMODE(key_file.stat().st_mode) == 0o600


def test_ensure_credential_key_noop_when_env_set(tmp_path, monkeypatch):
    """env 已设时不落盘、不提示。"""
    from cryptography.fernet import Fernet
    from echo_agent.cli.setup import _ensure_credential_key
    monkeypatch.setenv("ECHO_AGENT_CREDENTIAL_KEY", Fernet.generate_key().decode())
    _ensure_credential_key(tmp_path)
    assert not (tmp_path / ".credential_key").exists()


def test_ensure_credential_key_no_reprompt_when_exists(tmp_path, monkeypatch, capsys):
    """key 文件已存在时不重复打印生成提示。"""
    from echo_agent.cli.setup import _ensure_credential_key
    monkeypatch.delenv("ECHO_AGENT_CREDENTIAL_KEY", raising=False)
    _ensure_credential_key(tmp_path)       # 首次生成
    capsys.readouterr()                    # 清空已捕获输出
    _ensure_credential_key(tmp_path)        # 第二次：已存在
    out = capsys.readouterr().out
    assert "0600" not in out and "credential" not in out.lower()
