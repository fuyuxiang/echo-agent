"""Tests for newly added security features:
- ANSI-C quoting decode
- Shell variable expansion
- Process substitution extraction
- Heredoc body extraction
- Process tool signal/stdin guards
- SSRF internal URL detection
- Credential file read-deny
- Smart approval exact match
"""
from __future__ import annotations

import pytest

from echo_agent.security.normalizer import (
    decode_ansi_c_quoting,
    expand_shell_variables,
    normalize_command,
)
from echo_agent.security.tokenizer import ShellTokenizer
from echo_agent.security.guards import (
    _is_internal_url,
    evaluate_tool_call,
    scan_shell_command,
)
from echo_agent.security.path_policy import check_read
from echo_agent.config.schema import Config, ExecToolConfig, ToolsConfig


# ── ANSI-C Quoting ──────────────────────────────────────────────────────────


class TestAnsiCQuoting:
    def test_hex_decode(self):
        assert decode_ansi_c_quoting(r"$'\x72\x6d'") == "rm"

    def test_octal_decode(self):
        assert decode_ansi_c_quoting(r"$'\162\155'") == "rm"

    def test_simple_escapes(self):
        assert decode_ansi_c_quoting(r"$'\n\t'") == "\n\t"

    def test_no_ansi_c_passthrough(self):
        assert decode_ansi_c_quoting("normal command") == "normal command"

    def test_mixed_content(self):
        result = decode_ansi_c_quoting(r"echo $'\x68\x65\x6c\x6c\x6f'")
        assert "hello" in result

    def test_normalize_catches_ansi_c_rm(self):
        normalized = normalize_command(r"$'\x72\x6d' -rf /")
        findings = scan_shell_command(normalized)
        assert any(f.key == "root_rm" for f in findings)


# ── Shell Variable Expansion ─────────────────────────────────────────────────


class TestShellVariableExpansion:
    def test_simple_var(self):
        assert expand_shell_variables("$HOME/file") == "HOME/file"

    def test_brace_var(self):
        assert expand_shell_variables("${cmd}") == "cmd"

    def test_brace_with_default(self):
        assert expand_shell_variables("${cmd:-rm}") == "cmd"

    def test_no_var_passthrough(self):
        assert expand_shell_variables("plain text") == "plain text"

    def test_normalize_surfaces_var_name(self):
        normalized = normalize_command("${rm} -rf /")
        assert "rm" in normalized


# ── Process Substitution Extraction ──────────────────────────────────────────


class TestProcessSubstitution:
    def setup_method(self):
        self.tokenizer = ShellTokenizer()

    def test_input_process_substitution(self):
        result = self.tokenizer.tokenize("diff <(cat /etc/passwd) <(cat /etc/shadow)")
        assert any("cat /etc/passwd" in cmd for cmd in result)
        assert any("cat /etc/shadow" in cmd for cmd in result)

    def test_output_process_substitution(self):
        result = self.tokenizer.tokenize("tee >(wc -l) >(cat > output.txt)")
        assert any("wc -l" in cmd for cmd in result)

    def test_no_process_substitution(self):
        result = self.tokenizer.tokenize("echo hello")
        assert "echo hello" in result


# ── Heredoc Extraction ───────────────────────────────────────────────────────


class TestHeredocExtraction:
    def setup_method(self):
        self.tokenizer = ShellTokenizer()

    def test_heredoc_body_extracted(self):
        cmd = "cat <<EOF\nrm -rf /\nEOF"
        result = self.tokenizer.tokenize(cmd)
        assert any("rm -rf /" in sub for sub in result)

    def test_heredoc_no_match_without_delimiter(self):
        cmd = "echo hello"
        result = self.tokenizer.tokenize(cmd)
        assert not any("rm" in sub for sub in result)


# ── SSRF Internal URL Detection ──────────────────────────────────────────────


class TestInternalUrlDetection:
    def test_localhost(self):
        assert _is_internal_url("http://localhost:8080/api")

    def test_127_range(self):
        assert _is_internal_url("http://127.0.0.1:9200/_search")

    def test_10_private(self):
        assert _is_internal_url("http://10.0.0.5/internal")

    def test_172_private(self):
        assert _is_internal_url("http://172.16.0.1/admin")

    def test_192_168_private(self):
        assert _is_internal_url("http://192.168.1.1/config")

    def test_dot_local(self):
        assert _is_internal_url("http://myservice.local/health")

    def test_dot_internal(self):
        assert _is_internal_url("http://api.internal/secret")

    def test_public_url_not_internal(self):
        assert not _is_internal_url("https://www.google.com")

    def test_empty_url(self):
        assert not _is_internal_url("")

    def test_ipv6_loopback(self):
        assert _is_internal_url("http://[::1]:3000/")


# ── Process Tool Signal/Stdin Guards ─────────────────────────────────────────


class TestProcessToolGuards:
    def _config(self, network="allow"):
        return Config(
            tools=ToolsConfig(profile="full"),
            execution={"network_policy": network},
        )

    def test_signal_kill_requires_approval(self):
        decision = evaluate_tool_call(
            self._config(),
            "process",
            {"action": "signal", "signal": "SIGKILL"},
        )
        assert decision.action in ("ask", "deny")
        assert decision.pattern_key == "process_signal"

    def test_signal_sigcont_allowed(self):
        decision = evaluate_tool_call(
            self._config(),
            "process",
            {"action": "signal", "signal": "SIGCONT"},
        )
        assert decision.action == "allow"

    def test_stdin_with_dangerous_content_denied(self):
        decision = evaluate_tool_call(
            self._config(),
            "process",
            {"action": "stdin", "data": "rm -rf /"},
        )
        assert decision.action == "deny"

    def test_stdin_with_safe_content_allowed(self):
        decision = evaluate_tool_call(
            self._config(),
            "process",
            {"action": "stdin", "data": "hello world"},
        )
        assert decision.action == "allow"

    def test_process_disabled(self):
        cfg = Config(
            tools=ToolsConfig(profile="full", exec=ExecToolConfig(enabled=False)),
        )
        decision = evaluate_tool_call(cfg, "process", {"action": "signal", "signal": "9"})
        assert decision.action == "deny"
        assert decision.pattern_key == "tool_disabled"

    def test_web_fetch_internal_url_flagged(self):
        decision = evaluate_tool_call(
            self._config(),
            "web_fetch",
            {"url": "http://169.254.169.254/latest/meta-data/"},
        )
        assert decision.action in ("ask", "deny")
        assert decision.pattern_key == "ssrf_internal"

    def test_web_fetch_public_url_allowed(self):
        decision = evaluate_tool_call(
            self._config(),
            "web_fetch",
            {"url": "https://api.github.com/repos"},
        )
        assert decision.action == "allow"


# ── Credential File Read-Deny ────────────────────────────────────────────────


class TestCredentialReadDeny:
    def test_ssh_private_key_read_denied(self, tmp_path):
        import os
        home = os.path.expanduser("~")
        result = check_read(f"{home}/.ssh/id_rsa", str(tmp_path))
        assert result is not None
        assert "credential" in result.lower() or "denied" in result.lower()

    def test_env_file_read_denied(self, tmp_path):
        result = check_read(str(tmp_path / ".env"), str(tmp_path))
        assert result is not None
        assert "credential" in result.lower()

    def test_normal_file_read_allowed(self, tmp_path):
        result = check_read(str(tmp_path / "readme.txt"), str(tmp_path))
        assert result is None


# ── Smart Approval Exact Match ───────────────────────────────────────────────


class TestSmartApprovalParsing:
    """Verify the response parser uses first-word exact match, not substring."""

    @pytest.mark.asyncio
    async def test_approve_exact_word(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="APPROVE")
        )
        result = await smart_approve("exec", "ls", "test", provider)
        assert result == "approve"

    @pytest.mark.asyncio
    async def test_approve_with_trailing_text(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="APPROVE - this is safe")
        )
        result = await smart_approve("exec", "ls", "test", provider)
        assert result == "approve"

    @pytest.mark.asyncio
    async def test_embedded_approve_not_matched(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="I would APPROVE this but let me think")
        )
        result = await smart_approve("exec", "ls", "test", provider)
        assert result == "escalate"

    @pytest.mark.asyncio
    async def test_deny_exact(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="DENY")
        )
        result = await smart_approve("exec", "rm -rf /", "destructive", provider)
        assert result == "deny"

    @pytest.mark.asyncio
    async def test_empty_response_escalates(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="")
        )
        result = await smart_approve("exec", "cmd", "reason", provider)
        # Empty content is now treated as a provider outage signature
        # and surfaces 'unavailable' instead of silently escalating.
        assert result == "unavailable"


class TestSmartApprovalUnavailable:
    """Provider outage (empty/None/exception) → 'unavailable', not silent escalate."""

    @pytest.mark.asyncio
    async def test_empty_content_is_unavailable(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=""))
        result = await smart_approve("exec", "curl x", "test", provider)
        assert result == "unavailable"

    @pytest.mark.asyncio
    async def test_none_content_is_unavailable(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=None))
        result = await smart_approve("exec", "curl x", "test", provider)
        assert result == "unavailable"

    @pytest.mark.asyncio
    async def test_exception_is_unavailable(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(side_effect=RuntimeError("provider down"))
        result = await smart_approve("exec", "curl x", "test", provider)
        assert result == "unavailable"

    @pytest.mark.asyncio
    async def test_nonempty_unrecognized_still_escalates(self):
        from unittest.mock import AsyncMock, MagicMock
        from echo_agent.security.smart_approval import smart_approve

        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="I would APPROVE this but let me think")
        )
        result = await smart_approve("exec", "ls", "test", provider)
        assert result == "escalate"
