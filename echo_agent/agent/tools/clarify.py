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

_INTERACTIVE_OPTIONS_DESCRIPTION = (
    "The choices to offer the user, one per entry. Provide these whenever the "
    "user should pick from a fixed set — they become an interactive, clickable "
    "picker. Omit only for a genuinely open-ended (free-text) question."
)
# Same parameter, told the truth for a text-only channel. The model reads the
# whole function schema, so leaving the picker promise here would contradict
# TEXT_DESCRIPTION and re-open the bug from the parameter side.
_TEXT_OPTIONS_DESCRIPTION = (
    "The choices to offer the user, one per entry. Provide these whenever the "
    "user should pick from a fixed set. They are rendered as a plain text list "
    "labelled A, B, C..., which the user answers by replying with a letter — so "
    "keep each option short. Omit only for a genuinely open-ended (free-text) question."
)


def _build_parameters(options_description: str) -> dict[str, Any]:
    """Build the clarify parameter schema with a channel-appropriate options text.

    Returns a fresh dict each call so per-channel variants never alias — and so
    no caller can mutate one channel's schema into another's."""
    return {
        "type": "object",
        "properties": {
            "question": {"type": "string", "description": "The question to ask the user."},
            "options": {
                "type": "array",
                "items": {"type": "string"},
                "description": options_description,
            },
        },
        "required": ["question"],
    }


class ClarifyTool(Tool):
    name = "clarify"
    INTERACTIVE_DESCRIPTION = (
        "Ask the user a clarifying question, optionally with multiple-choice options. "
        "This is the ONLY way to present the user with selectable choices: options passed "
        "here render as an interactive picker (number keys / arrows+enter) that the user can "
        "actually click. Always use this tool — with the choices in `options` — instead of "
        "writing a numbered/bulleted list of options into your reply text, because plain-text "
        "options are not selectable."
    )
    # This channel cannot render a choice control. Say so plainly: the model
    # then shortens the options and phrases them to be answered by letter,
    # instead of offering a picker the user will never see.
    TEXT_DESCRIPTION = (
        "Ask the user a clarifying question, optionally with multiple-choice options. "
        "On THIS channel options are shown as a plain text list only — the user cannot tap "
        "or select them. The user answers by replying with the option letter (A, B, C...) or by "
        "typing an answer in their own words, and their next message is routed back to you "
        "as the answer. Keep options short and easy to say out loud, and offer only a few; "
        "long or numerous options are hard to answer this way. Use this tool rather than "
        "writing the options into your reply text, so the answer is bound to the question. "
        "Slash commands such as /approve and /deny still work on this channel; only the "
        "clarify choices lack tappable controls."
    )
    description = INTERACTIVE_DESCRIPTION
    INTERACTIVE_PARAMETERS = _build_parameters(_INTERACTIVE_OPTIONS_DESCRIPTION)
    TEXT_PARAMETERS = _build_parameters(_TEXT_OPTIONS_DESCRIPTION)
    parameters = INTERACTIVE_PARAMETERS
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
        # Letters, matching AgentLoop._maybe_bind_im_clarify_answer: the user
        # sees the same labels the model is later told the options were. Letters
        # also survive dictation better than digits, which collide with numbers
        # appearing inside the option text.
        choices = "\n".join(f"  {chr(65 + i)}. {opt}" for i, opt in enumerate(options))
        return f"{question}\n{choices}\n（直接回复选项字母或你的答案即可）"

    @staticmethod
    def _channel_has_picker(channel: str | None) -> bool:
        from echo_agent.agent.cognitive_emitter import should_emit_cognitive

        # should_emit_cognitive is the existing answer to "can this channel
        # receive structured frames" — a clarify_request frame is exactly what
        # the picker is built from. Reusing it keeps one definition of the
        # capability instead of two that can drift. Description and parameters
        # both route through here so they can never disagree with each other.
        return should_emit_cognitive(channel or "")

    def description_for_channel(self, channel: str | None) -> str:
        return self.INTERACTIVE_DESCRIPTION if self._channel_has_picker(channel) else self.TEXT_DESCRIPTION

    def parameters_for_channel(self, channel: str | None) -> dict[str, Any]:
        return self.INTERACTIVE_PARAMETERS if self._channel_has_picker(channel) else self.TEXT_PARAMETERS

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
