"""Phase 3 — LLM-powered summary generation.

Serializes the middle segment into a compact representation and asks the
LLM to produce a structured summary preserving key decisions, file paths,
errors, user preferences, and task progress.
"""

from __future__ import annotations

import time
from typing import Any

from loguru import logger

from echo_agent.agent.compression.types import CompressionStats
from echo_agent.models.provider import LLMProvider

_CONTENT_TRUNCATE_THRESHOLD = 6000
_CONTENT_HEAD_CHARS = 4000
_CONTENT_TAIL_CHARS = 1500
_ARG_TRUNCATE_THRESHOLD = 1500
_ARG_TRUNCATE_TO = 1200

_SUMMARY_PROMPT = """\
You are a conversation compressor. Summarize the following conversation segment \
into a concise reference document that preserves all information needed to \
continue the conversation without loss of context.

MUST preserve:
- Key decisions made and their rationale
- File paths, function names, and code identifiers mentioned
- Error messages and their resolutions
- User preferences and constraints stated
- Current task progress and next steps
- Tool results that informed decisions

Format as a structured summary with sections. Be concise but complete. \
Do NOT include pleasantries or meta-commentary about the summarization itself.

{focus_section}

TOKEN BUDGET: Aim for approximately {budget_tokens} tokens.

---
CONVERSATION SEGMENT TO SUMMARIZE:

{serialized}"""


class LLMSummarizer:

    def __init__(
        self,
        provider: LLMProvider,
        summary_model: str,
        default_model: str,
        summary_target_ratio: float,
        summary_min_tokens: int,
        summary_max_tokens: int,
        cooldown_seconds: int,
    ):
        self._provider = provider
        self._model = summary_model or default_model
        self._target_ratio = summary_target_ratio
        self._min_tokens = summary_min_tokens
        self._max_tokens = summary_max_tokens
        self._cooldown = cooldown_seconds

    async def summarize(
        self,
        middle_messages: list[dict[str, Any]],
        focus_topic: str,
        stats: CompressionStats,
        token_estimator: Any,
    ) -> str | None:
        if not middle_messages:
            return None

        if stats.last_summary_failure_at is not None:
            elapsed = time.time() - stats.last_summary_failure_at
            if elapsed < self._cooldown:
                logger.info(
                    "Summary generation in cooldown ({:.0f}s remaining)",
                    self._cooldown - elapsed,
                )
                return None

        serialized = self._serialize(middle_messages)
        content_tokens = len(serialized) // 4
        budget = self._compute_budget(content_tokens)

        focus_section = ""
        if focus_topic:
            focus_section = f"Current focus topic: {focus_topic}\nPrioritize information related to this topic."

        prompt = _SUMMARY_PROMPT.format(
            focus_section=focus_section,
            budget_tokens=budget,
            serialized=serialized,
        )

        try:
            response = await self._provider.chat_with_retry(
                messages=[{"role": "user", "content": prompt}],
                model=self._model,
                max_tokens=budget + 500,
            )
            if response.finish_reason == "error" or not response.content:
                logger.warning("Summary generation failed: {}", response.content)
                stats.last_summary_failure_at = time.time()
                return None

            return response.content.strip()

        except Exception as e:
            logger.error("Summary generation exception: {}", e)
            stats.last_summary_failure_at = time.time()
            return None

    def _compute_budget(self, content_tokens: int) -> int:
        budget = int(content_tokens * self._target_ratio)
        return max(self._min_tokens, min(budget, self._max_tokens))

    def _serialize(self, messages: list[dict[str, Any]]) -> str:
        lines: list[str] = []
        turn = 0

        for msg in messages:
            role = msg.get("role", "unknown").upper()
            content = msg.get("content", "")

            if role == "USER":
                turn += 1

            if role == "ASSISTANT" and msg.get("tool_calls"):
                calls_desc = []
                for tc in msg.get("tool_calls", []):
                    func = tc.get("function", {})
                    name = func.get("name", "?")
                    args = func.get("arguments", "")
                    if isinstance(args, str) and len(args) > _ARG_TRUNCATE_THRESHOLD:
                        args = args[:_ARG_TRUNCATE_TO] + "..."
                    calls_desc.append(f"  → {name}({args[:200]})")

                text_part = self._truncate_content(content) if content else ""
                tool_part = "\n".join(calls_desc)
                if text_part:
                    lines.append(f"[T{turn}][{role}]: {text_part}")
                lines.append(f"[T{turn}][{role} TOOL_CALLS]:\n{tool_part}")
                continue

            if role == "TOOL":
                name = msg.get("name", "tool")
                content_str = self._truncate_content(content)
                lines.append(f"[T{turn}][TOOL:{name}]: {content_str}")
                continue

            content_str = self._truncate_content(content)
            if content_str:
                lines.append(f"[T{turn}][{role}]: {content_str}")

        return "\n\n".join(lines)

    def _truncate_content(self, content: Any) -> str:
        if not content:
            return ""
        if isinstance(content, list):
            parts = []
            for block in content:
                parts.append(str(block.get("text", "")))
            text = "\n".join(parts)
        else:
            text = str(content)

        if len(text) <= _CONTENT_TRUNCATE_THRESHOLD:
            return text

        head = text[:_CONTENT_HEAD_CHARS]
        tail = text[-_CONTENT_TAIL_CHARS:]
        omitted = len(text) - _CONTENT_HEAD_CHARS - _CONTENT_TAIL_CHARS
        return f"{head}\n\n[...{omitted} chars omitted...]\n\n{tail}"
