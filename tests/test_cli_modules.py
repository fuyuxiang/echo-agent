"""Tests for CLI utilities — colors, status, prompt."""

from __future__ import annotations

from unittest.mock import patch, MagicMock


from echo_agent.cli.colors import Colors, color, print_header, print_success, print_info, print_warning, print_error


# ══════════════════════════════════════════════════════════════════════════════
# color() function
# ══════════════════════════════════════════════════════════════════════════════


class TestColorFunction:
    def test_no_codes(self):
        assert color("hello") == "hello"

    def test_single_code(self):
        result = color("text", Colors.RED)
        assert result == f"{Colors.RED}text{Colors.RESET}"

    def test_multiple_codes(self):
        result = color("text", Colors.BOLD, Colors.CYAN)
        assert result == f"{Colors.BOLD}{Colors.CYAN}text{Colors.RESET}"

    def test_empty_text(self):
        result = color("", Colors.GREEN)
        assert result == f"{Colors.GREEN}{Colors.RESET}"


# ══════════════════════════════════════════════════════════════════════════════
# print_* functions
# ══════════════════════════════════════════════════════════════════════════════


class TestPrintFunctions:
    def test_print_header(self, capsys):
        print_header("Test Header")
        captured = capsys.readouterr()
        assert "Test Header" in captured.out

    def test_print_success(self, capsys):
        print_success("All good")
        captured = capsys.readouterr()
        assert "All good" in captured.out
        assert Colors.GREEN in captured.out

    def test_print_info(self, capsys):
        print_info("Some info")
        captured = capsys.readouterr()
        assert "Some info" in captured.out

    def test_print_warning(self, capsys):
        print_warning("Watch out")
        captured = capsys.readouterr()
        assert "Watch out" in captured.out
        assert Colors.YELLOW in captured.out

    def test_print_error(self, capsys):
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
