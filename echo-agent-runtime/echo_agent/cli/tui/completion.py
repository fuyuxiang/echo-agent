"""Slash-command catalog and pure completion filters. Server commands mirror
the ones the gateway intercepts before the session lock — the approval trio
(loop.py ``_is_approval_command``) plus ``/clarify`` (``_is_clarify_command``);
everything else is a client-local command executed inside the TUI and never
sent upstream."""

from __future__ import annotations

from dataclasses import dataclass

from echo_agent.cli.i18n import t


@dataclass(frozen=True)
class SlashCommand:
    name: str
    arg_template: str
    desc: str
    scope: str  # "server" | "local"
    takes_args: bool


COMMANDS: tuple[SlashCommand, ...] = (
    SlashCommand("/approve", "<id> [session|always]", t("attach.commands.approve"), "server", True),
    SlashCommand("/deny", "<id> [reason]", t("attach.commands.deny"), "server", True),
    SlashCommand("/approvals", "", t("attach.commands.approvals"), "server", False),
    SlashCommand("/clarify", "<id> <answer>", t("attach.commands.clarify"), "server", True),
    SlashCommand("/help", "", t("attach.commands.help"), "local", False),
    SlashCommand("/clear", "", t("attach.commands.clear"), "local", False),
    SlashCommand("/copy", "[all]", t("attach.commands.copy"), "local", True),
    SlashCommand(
        "/details",
        t("attach.args.details"),
        t("attach.commands.details"),
        "local",
        True,
    ),
    SlashCommand(
        "/save",
        t("attach.args.save"),
        t("attach.commands.save"),
        "local",
        True,
    ),
    SlashCommand("/theme", "[light|dark]", t("attach.commands.theme"), "local", True),
    SlashCommand("/reconnect", "", t("attach.commands.reconnect"), "local", False),
    SlashCommand("/status", "[event_id]", t("attach.commands.status"), "local", True),
    SlashCommand("/quit", "", t("attach.commands.quit"), "local", False),
)


def help_text() -> str:
    """Rich-markup help listing every command, grouped by scope. Rendered by the
    /help local command so the banner's '/help 查看命令' promise is real."""
    lines = [f"[b]{t('attach.help.title')}[/b]"]
    server = [c for c in COMMANDS if c.scope == "server"]
    local = [c for c in COMMANDS if c.scope == "local"]
    for title, group in ((t("attach.help.local"), local), (t("attach.help.server"), server)):
        lines.append(f"[$text-muted]{title}[/]")
        for c in group:
            arg = f" [$text-muted]{c.arg_template}[/]" if c.arg_template else ""
            lines.append(f"  [$primary]{c.name}[/]{arg}  {c.desc}")
    return "\n".join(lines)


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
