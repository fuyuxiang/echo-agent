"""Context builder — assembles system prompt, memory, history, and runtime info.

Handles layered injection:
  1. System prompt (identity + bootstrap files)
  2. User profile / environment memory
  3. Skills context
  4. Runtime metadata (time, channel, chat)
  5. Conversation history (with sliding window + summary compression)
  6. Retrieval-augmented context from memory search
"""

from __future__ import annotations

import platform
import time
from datetime import datetime
from pathlib import Path
from typing import Any

from loguru import logger


_SKILLS_GUIDANCE = """\
You have access to a self-learning skill system. Skills are reusable procedures captured from past tasks.

- Use `skills_list` to see available skills before starting a task.
- Use `skill_view` to load full instructions when a skill matches the current task.
- After completing a non-trivial task, consider using `skill_manage` to create or update a skill \
if the approach involved trial-and-error, domain knowledge, or steps that would help with similar future tasks.
- Skills should capture the procedure, pitfalls, and verification steps — not just the final answer.
- Use YAML frontmatter with at least 'name' and 'description' fields."""

_MEMORY_GUIDANCE = """\
You have persistent memory across sessions. Use the `memory` tool to manage it.

- Save user preferences, habits, and communication style as "user" memories.
- Save project facts, conventions, tool configs, and domain knowledge as "environment" memories.
- Use `search` to check if relevant memories exist before starting a task.
- Use `replace` to update outdated information rather than adding duplicates.
- Use `remove` to delete information that is no longer accurate.
- Only save information that would be useful in future conversations — skip trivial or one-off details."""


def build_memory_context(memory_store: Any, snapshot: str = "") -> str:
    """Build the memory section for the system prompt."""
    parts: list[str] = [_MEMORY_GUIDANCE]
    if snapshot:
        parts.append(snapshot)
    elif memory_store is not None:
        try:
            snap = memory_store.get_snapshot()
            if snap:
                parts.append(snap)
        except Exception:
            pass
    return "\n\n".join(parts) if len(parts) > 1 else parts[0]


def build_skills_context(skill_store: Any) -> str:
    """Build a compact skills section for the system prompt."""
    if skill_store is None:
        return ""
    try:
        skills = skill_store.list_all()
    except Exception:
        return ""
    if not skills:
        return _SKILLS_GUIDANCE + "\n\nNo skills available yet."
    lines = [_SKILLS_GUIDANCE, "", "Available skills:"]
    for s in skills:
        tag = f" [{s.category}]" if s.category else ""
        lines.append(f"  - {s.name}{tag}: {s.description}")
    return "\n".join(lines)


class ContextBuilder:
    BOOTSTRAP_FILES = ("AGENTS.md", "SOUL.md", "USER.md", "TOOLS.md")
    _RUNTIME_TAG = "[Runtime Context]"

    def __init__(self, workspace: Path, agent_name: str = "Echo"):
        self.workspace = workspace
        self.agent_name = agent_name

    def build_system_prompt(
        self,
        memory_context: str = "",
        skills_context: str = "",
        user_profile: str = "",
        env_context: str = "",
        custom_instructions: str = "",
    ) -> str:
        parts = [self._identity()]

        bootstrap = self._load_bootstrap_files()
        if bootstrap:
            parts.append(bootstrap)

        if user_profile:
            parts.append(f"# User Profile\n\n{user_profile}")

        if env_context:
            parts.append(f"# Environment Context\n\n{env_context}")

        if memory_context:
            parts.append(f"# Memory\n\n{memory_context}")

        if skills_context:
            parts.append(f"# Active Skills\n\n{skills_context}")

        if custom_instructions:
            parts.append(f"# Custom Instructions\n\n{custom_instructions}")

        return "\n\n---\n\n".join(parts)

    def build_messages(
        self,
        history: list[dict[str, Any]],
        current_message: str,
        media: list[str] | None = None,
        channel: str | None = None,
        chat_id: str | None = None,
        system_prompt: str = "",
        retrieval_context: str = "",
    ) -> list[dict[str, Any]]:
        runtime = self._runtime_context(channel, chat_id)
        user_content = current_message
        if retrieval_context:
            user_content = f"[Retrieved Context]\n{retrieval_context}\n\n{current_message}"

        merged_user = f"{runtime}\n\n{user_content}"

        messages: list[dict[str, Any]] = []
        if system_prompt:
            messages.append({"role": "system", "content": system_prompt})
        messages.extend(history)
        messages.append({"role": "user", "content": merged_user})
        return messages

    def _identity(self) -> str:
        sys_info = platform.system()
        runtime = f"{'macOS' if sys_info == 'Darwin' else sys_info} {platform.machine()}, Python {platform.python_version()}"
        ws = str(self.workspace.resolve())
        return f"""# {self.agent_name}

You are {self.agent_name}, a helpful AI assistant.

## Runtime
{runtime}

## Workspace
{ws}

## Guidelines
- State intent before tool calls, never predict results.
- Read files before modifying them.
- Ask for clarification when the request is ambiguous."""

    def _runtime_context(self, channel: str | None, chat_id: str | None) -> str:
        now = datetime.now().strftime("%Y-%m-%d %H:%M (%A)")
        tz = time.strftime("%Z") or "UTC"
        lines = [f"Current Time: {now} ({tz})"]
        if channel and chat_id:
            lines.extend([f"Channel: {channel}", f"Chat ID: {chat_id}"])
        return self._RUNTIME_TAG + "\n" + "\n".join(lines)

    def _load_bootstrap_files(self) -> str:
        parts = []
        for name in self.BOOTSTRAP_FILES:
            path = self.workspace / name
            if path.exists():
                content = path.read_text(encoding="utf-8")
                parts.append(f"## {name}\n\n{content}")
        return "\n\n".join(parts)
