"""Clarify tool — ask the user a clarifying question and block until answered.

On the CLI channel the agent truly blocks on ClarifyManager.wait_for_answer
until the user picks an option (or types a free-text answer). On non-CLI
channels (IM), clarify degrades to returning the question + options as text
without blocking — blocking IM clarify is a future iteration (see spec)."""

from __future__ import annotations

from typing import Any

from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult

CLI_CHANNEL = "gateway:cli"


class ClarifyTool(Tool):
    name = "clarify"
    description = "Ask the user a clarifying question, optionally with multiple-choice options."
    parameters = {
        "type": "object",
        "properties": {
            "question": {"type": "string", "description": "The question to ask the user."},
            "options": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional list of choices for the user.",
            },
        },
        "required": ["question"],
    }
    # A CLI clarify blocks until the user answers, with no timeout. The registry
    # wraps execute() in asyncio.wait_for(timeout=timeout_seconds), so this
    # ceiling must be far above any realistic human response time.
    timeout_seconds = 86400

    def __init__(self, manager: ClarifyManager):
        self._manager = manager

    @staticmethod
    def _render_text(question: str, options: list[str]) -> str:
        if not options:
            return question
        choices = "\n".join(f"  {i + 1}. {opt}" for i, opt in enumerate(options))
        return f"{question}\n{choices}"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        question = params["question"]
        options = params.get("options", []) or []
        channel = ctx.channel if ctx else ""

        # Non-CLI (IM) channels: degrade to a text echo, do not block. Blocking
        # IM clarify needs different semantics (timeout / lock release) — future.
        if channel != CLI_CHANNEL:
            return ToolResult(
                output=self._render_text(question, options),
                metadata={"type": "clarify", "question": question, "options": options},
            )

        # CLI: the pipeline pre-registers and injects _clarify_id; if absent
        # (defensive), self-register so the tool still blocks correctly.
        clarify_id = params.get("_clarify_id")
        if not clarify_id:
            req = self._manager.request(question, options, user_id=(ctx.user_id if ctx else ""))
            clarify_id = req.id

        answer, interrupted = await self._manager.wait_for_answer(clarify_id)
        if interrupted:
            # Session was cancelled while waiting. Let the agent wrap up.
            return ToolResult(
                success=True,
                output="用户未回应(会话中断)。",
                metadata={"type": "clarify", "clarify_id": clarify_id, "interrupted": True},
            )
        # An empty-but-not-interrupted answer is a real (blank) user reply — hand
        # it back as-is and let the model decide, rather than faking an interrupt.
        return ToolResult(
            output=answer,
            metadata={"type": "clarify", "clarify_id": clarify_id, "question": question},
        )
