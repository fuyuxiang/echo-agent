"""Shell command tokenizer — splits compound commands into sub-commands.

Handles pipes, semicolons, &&, ||, backticks, $() substitution, process
substitution <() / >(), and heredoc <<EOF blocks so that security guards
can inspect each sub-command individually.

Uses stack-based parsing instead of regex to correctly handle nested
command substitutions like $(echo $(whoami)).
"""

from __future__ import annotations

import re
import shlex


_COMPOUND_SPLIT_RE = re.compile(
    r"""
    \s*(?:
        ;\s*         |   # semicolons
        &&\s*        |   # logical AND
        \|\|\s*      |   # logical OR
        \|\s*            # pipe
    )
    """,
    re.VERBOSE,
)

_PROCESS_SUBST_RE = re.compile(r"[<>]\(([^)]+)\)")

_HEREDOC_RE = re.compile(
    r"<<-?\s*['\"]?(\w+)['\"]?\s*\n(.*?)\n\s*\1",
    re.DOTALL,
)


def _extract_subshells(cmd: str) -> list[str]:
    """Stack-based extraction for nested $() and backtick substitutions."""
    results: list[str] = []
    i = 0
    length = len(cmd)
    while i < length:
        if i < length - 1 and cmd[i] == "$" and cmd[i + 1] == "(":
            depth = 1
            start = i + 2
            j = start
            while j < length and depth > 0:
                if j < length - 1 and cmd[j] == "$" and cmd[j + 1] == "(":
                    depth += 1
                    j += 1
                elif cmd[j] == ")":
                    depth -= 1
                j += 1
            if depth == 0:
                inner = cmd[start : j - 1].strip()
                if inner:
                    results.append(inner)
                    results.extend(_extract_subshells(inner))
            i = j
        elif cmd[i] == "`":
            end = cmd.find("`", i + 1)
            if end > i:
                inner = cmd[i + 1 : end].strip()
                if inner:
                    results.append(inner)
                    results.extend(_extract_subshells(inner))
                i = end + 1
            else:
                i += 1
        else:
            i += 1
    return results


def _extract_process_substitutions(cmd: str) -> list[str]:
    """Extract commands from <(...) and >(...) process substitutions."""
    results: list[str] = []
    for m in _PROCESS_SUBST_RE.finditer(cmd):
        inner = m.group(1).strip()
        if inner:
            results.append(inner)
            results.extend(_extract_subshells(inner))
    return results


def _extract_heredoc_bodies(cmd: str) -> list[str]:
    """Extract body content from heredoc blocks for inspection."""
    results: list[str] = []
    for m in _HEREDOC_RE.finditer(cmd):
        body = m.group(2).strip()
        if body:
            results.append(body)
    return results


class ShellTokenizer:
    """Splits compound shell commands into individual sub-commands for analysis."""

    def tokenize(self, command: str) -> list[str]:
        """Split a compound command into individual sub-commands.

        Extracts commands from:
        - Pipe chains: cmd1 | cmd2
        - Sequential: cmd1 ; cmd2
        - Logical operators: cmd1 && cmd2, cmd1 || cmd2
        - Command substitution: $(cmd) and `cmd` (including nested)
        - Process substitution: <(cmd) and >(cmd)
        - Heredoc bodies: <<EOF ... EOF
        """
        sub_commands: list[str] = []

        sub_commands.extend(_extract_subshells(command))
        sub_commands.extend(_extract_process_substitutions(command))
        sub_commands.extend(_extract_heredoc_bodies(command))

        top_level = _COMPOUND_SPLIT_RE.split(command)
        for part in top_level:
            stripped = part.strip()
            if stripped:
                sub_commands.append(stripped)

        return sub_commands or [command.strip()]

    def split_tokens(self, command: str) -> list[str]:
        """Split command into shell tokens using shlex with fallback."""
        try:
            return shlex.split(command)
        except ValueError:
            return command.split()
