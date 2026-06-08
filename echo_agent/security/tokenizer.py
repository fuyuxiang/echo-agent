"""Shell command tokenizer — splits compound commands into sub-commands.

Handles pipes, semicolons, &&, ||, backticks, and $() substitution so that
security guards can inspect each sub-command individually.
"""

from __future__ import annotations

import re


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

_SUBSHELL_RE = re.compile(r"\$\(([^)]+)\)")
_BACKTICK_RE = re.compile(r"`([^`]+)`")


class ShellTokenizer:
    """Splits compound shell commands into individual sub-commands for analysis."""

    def tokenize(self, command: str) -> list[str]:
        """Split a compound command into individual sub-commands.

        Extracts commands from:
        - Pipe chains: cmd1 | cmd2
        - Sequential: cmd1 ; cmd2
        - Logical operators: cmd1 && cmd2, cmd1 || cmd2
        - Command substitution: $(cmd) and `cmd`
        """
        sub_commands: list[str] = []

        for m in _SUBSHELL_RE.finditer(command):
            inner = m.group(1).strip()
            if inner:
                sub_commands.append(inner)

        for m in _BACKTICK_RE.finditer(command):
            inner = m.group(1).strip()
            if inner:
                sub_commands.append(inner)

        top_level = _COMPOUND_SPLIT_RE.split(command)
        for part in top_level:
            stripped = part.strip()
            if stripped:
                cleaned = _SUBSHELL_RE.sub("__SUBSHELL__", stripped)
                cleaned = _BACKTICK_RE.sub("__BACKTICK__", cleaned)
                cleaned = cleaned.replace("__SUBSHELL__", "").replace("__BACKTICK__", "").strip()
                if cleaned:
                    sub_commands.append(stripped)
                elif stripped:
                    sub_commands.append(stripped)

        return sub_commands or [command.strip()]
