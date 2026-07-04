"""Slash-command catalog and pure completion filters. Server commands mirror
the ONLY three the gateway recognizes (loop.py:1194); everything else is a
client-local command executed inside the TUI and never sent upstream."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class SlashCommand:
    name: str
    arg_template: str
    desc: str
    scope: str  # "server" | "local"
    takes_args: bool


COMMANDS: tuple[SlashCommand, ...] = (
    SlashCommand("/approve", "<id> [session|always]", "批准待确认操作", "server", True),
    SlashCommand("/deny", "<id> [原因]", "拒绝待确认操作", "server", True),
    SlashCommand("/approvals", "", "列出待批操作", "server", False),
    SlashCommand("/clear", "", "清空当前转录流", "local", False),
    SlashCommand("/quit", "", "退出", "local", False),
)


def filter_commands(text: str) -> list[SlashCommand]:
    if not text.startswith("/"):
        return []
    # A space means the command name is finalized and the user has moved on to
    # arguments (e.g. "/approve 5") — the name-completion panel must not reopen.
    if any(ch.isspace() for ch in text):
        return []
    prefix = text.lower()
    return [c for c in COMMANDS if c.name.startswith(prefix)]


def completion_insert(cmd: SlashCommand) -> str:
    return f"{cmd.name} " if cmd.takes_args else cmd.name
