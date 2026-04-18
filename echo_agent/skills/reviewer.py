"""Background skill reviewer — auto-creates/updates skills after task completion.

Spawns a lightweight LLM call that reviews the conversation and decides whether
to capture a reusable skill. Runs in the background so it doesn't block the user.
"""

from __future__ import annotations

import json
from typing import Any

from loguru import logger

from echo_agent.models.provider import LLMProvider
from echo_agent.skills.store import SkillStore

_REVIEW_PROMPT = """\
Review the conversation above and consider saving or updating a skill if appropriate.

Focus on: was a non-trivial approach used to complete a task that required trial and error, \
changing course due to experiential findings, or domain-specific knowledge?

Guidelines:
- If a relevant skill already exists, update it with what you learned using the patch action.
- Otherwise, create a new skill if the approach is reusable across similar future tasks.
- Do NOT create skills for trivial or one-off tasks.
- Skills should capture the procedure, pitfalls, and verification steps — not just the final answer.
- Use YAML frontmatter with at least 'name' and 'description' fields.

If nothing is worth saving, simply respond with "No skill changes needed." and stop."""

_MAX_REVIEW_ITERATIONS = 8


class SkillReviewer:
    """Reviews conversations and auto-creates/updates skills."""

    def __init__(self, provider: LLMProvider, store: SkillStore, model: str = ""):
        self._provider = provider
        self._store = store
        self._model = model

    async def review(self, conversation: list[dict[str, Any]]) -> list[str]:
        """Run a background review. Returns list of action summaries."""
        actions: list[str] = []
        tool_defs = self._build_tool_defs()

        messages = list(conversation)
        messages.append({"role": "user", "content": _REVIEW_PROMPT})

        for _ in range(_MAX_REVIEW_ITERATIONS):
            try:
                response = await self._provider.chat_with_retry(
                    messages=messages,
                    tools=tool_defs,
                    model=self._model or None,
                )
            except Exception as e:
                logger.warning("Skill review LLM call failed: {}", e)
                break

            if response.content:
                messages.append({"role": "assistant", "content": response.content})

            if not response.has_tool_calls:
                break

            assistant_msg: dict[str, Any] = {"role": "assistant", "content": response.content or ""}
            assistant_msg["tool_calls"] = [tc.to_openai_format() for tc in response.tool_calls]
            if response.content:
                messages.pop()
            messages.append(assistant_msg)

            for tc in response.tool_calls:
                result = self._execute_tool(tc.name, tc.arguments)
                messages.append({"role": "tool", "tool_call_id": tc.id, "name": tc.name, "content": result})
                if not result.startswith("Error"):
                    actions.append(f"{tc.name}: {result}")

        if actions:
            logger.info("Skill review completed with {} action(s)", len(actions))
        return actions

    def _execute_tool(self, name: str, params: dict[str, Any]) -> str:
        """Synchronously execute a skill management tool call."""
        if name == "skill_manage":
            return self._handle_skill_manage(params)
        return f"Error: unknown tool '{name}'"

    def _handle_skill_manage(self, params: dict[str, Any]) -> str:
        action = params.get("action", "")
        skill_name = params.get("name", "")

        if action == "create":
            content = params.get("content", "")
            if not content:
                return "Error: content is required"
            err = self._store.create_skill(skill_name, content, category=params.get("category", ""))
            return err or f"Skill '{skill_name}' created."

        elif action == "edit":
            content = params.get("content", "")
            if not content:
                return "Error: content is required"
            err = self._store.update_skill(skill_name, content)
            return err or f"Skill '{skill_name}' updated."

        elif action == "patch":
            old_text = params.get("old_text", "")
            new_text = params.get("new_text", "")
            if not old_text:
                return "Error: old_text is required"
            err = self._store.patch_skill(skill_name, old_text, new_text, file_path=params.get("file_path", ""))
            return err or f"Skill '{skill_name}' patched."

        elif action == "delete":
            err = self._store.delete_skill(skill_name)
            return err or f"Skill '{skill_name}' deleted."

        elif action == "write_file":
            file_path = params.get("file_path", "")
            content = params.get("content", "")
            if not file_path or not content:
                return "Error: file_path and content required"
            err = self._store.write_file(skill_name, file_path, content)
            return err or f"File '{file_path}' written."

        elif action == "remove_file":
            file_path = params.get("file_path", "")
            if not file_path:
                return "Error: file_path required"
            err = self._store.remove_file(skill_name, file_path)
            return err or f"File '{file_path}' removed."

        return f"Error: unknown action '{action}'"

    @staticmethod
    def _build_tool_defs() -> list[dict[str, Any]]:
        return [
            {
                "type": "function",
                "function": {
                    "name": "skill_manage",
                    "description": "Create, edit, patch, or delete skills to capture reusable knowledge.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["create", "edit", "patch", "delete", "write_file", "remove_file"],
                            },
                            "name": {"type": "string"},
                            "category": {"type": "string"},
                            "content": {"type": "string"},
                            "file_path": {"type": "string"},
                            "old_text": {"type": "string"},
                            "new_text": {"type": "string"},
                        },
                        "required": ["action", "name"],
                    },
                },
            }
        ]
