"""Slash-command catalog and pure completion filters. Server commands mirror
the ones the gateway intercepts before the session lock — the approval trio
(loop.py ``_is_approval_command``) plus ``/clarify`` (``_is_clarify_command``);
everything else is a client-local command executed inside the TUI and never
sent upstream."""

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
    SlashCommand("/clarify", "<id> <答案>", "回复智能体的澄清提问", "server", True),
    SlashCommand("/help", "", "列出所有命令", "local", False),
    SlashCommand("/clear", "", "清空当前转录流", "local", False),
    SlashCommand("/copy", "[all]", "复制最近回复（all=整段对话）", "local", True),
    SlashCommand(
        "/details", "[思考|工具|状态 展开|折叠|精简|隐藏]",
        "查看或调整过程信息的显示程度", "local", True,
    ),
    SlashCommand("/save", "[路径]", "保存对话为 Markdown（默认存到 transcripts/）", "local", True),
    SlashCommand("/theme", "[light|dark]", "切换或查看亮/暗主题", "local", True),
    SlashCommand("/reconnect", "", "断线后重新连接网关", "local", False),
    SlashCommand("/quit", "", "退出", "local", False),
)


def help_text() -> str:
    """Rich-markup help listing every command, grouped by scope. Rendered by the
    /help local command so the banner's '/help 查看命令' promise is real."""
    lines = ["[b]可用命令[/b]"]
    server = [c for c in COMMANDS if c.scope == "server"]
    local = [c for c in COMMANDS if c.scope == "local"]
    for title, group in (("本地命令", local), ("服务端命令", server)):
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
