"""Memory consolidator — summarizes conversation chunks into long-term memory."""

from __future__ import annotations

import json
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.memory.store import MemoryStore

_SAVE_MEMORY_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "save_memory",
            "description": "Save memory consolidation result to persistent storage.",
            "parameters": {
                "type": "object",
                "properties": {
                    "history_entry": {
                        "type": "string",
                        "description": "Summary paragraph starting with [YYYY-MM-DD HH:MM].",
                    },
                    "memory_update": {
                        "type": "string",
                        "description": "Full updated long-term memory as markdown.",
                    },
                },
                "required": ["history_entry", "memory_update"],
            },
        },
    }
]


class MemoryConsolidator:
    """Consolidates conversation history into MEMORY.md + HISTORY.md via LLM."""

    _MAX_ROUNDS = 3

    def __init__(
        self,
        memory_store: MemoryStore,
        llm_call: Callable[..., Awaitable[Any]],
        context_window_tokens: int = 65536,
    ):
        self.store = memory_store
        self._llm_call = llm_call
        self.context_window_tokens = context_window_tokens

    async def consolidate_chunk(self, messages: list[dict[str, Any]]) -> bool:
        if not messages:
            return True

        current_memory = self.store.read_long_term()
        formatted = self._format_messages(messages)
        prompt = (
            "Process this conversation and call save_memory with your consolidation.\n\n"
            f"## Current Long-term Memory\n{current_memory or '(empty)'}\n\n"
            f"## Conversation to Process\n{formatted}"
        )

        try:
            response = await self._llm_call(
                messages=[
                    {"role": "system", "content": "You are a memory consolidation agent. Call save_memory."},
                    {"role": "user", "content": prompt},
                ],
                tools=_SAVE_MEMORY_TOOL,
                tool_choice={"type": "function", "function": {"name": "save_memory"}},
            )

            if not response.tool_calls:
                logger.warning("Consolidation: LLM did not call save_memory")
                return False

            args = response.tool_calls[0].arguments
            if isinstance(args, str):
                args = json.loads(args)

            history_entry = args.get("history_entry", "")
            memory_update = args.get("memory_update", "")

            if history_entry:
                self.store.append_history(history_entry)
            if memory_update:
                self.store.write_long_term(memory_update)

            logger.info("Memory consolidation complete: {} chars history, {} chars memory",
                        len(history_entry), len(memory_update))
            return True
        except Exception as e:
            logger.error("Memory consolidation failed: {}", e)
            return False

    def should_consolidate(self, session_message_count: int, last_consolidated: int) -> bool:
        unconsolidated = session_message_count - last_consolidated
        return unconsolidated >= 50

    def pick_boundary(self, messages: list[dict[str, Any]], start: int, target_tokens: int) -> int | None:
        """Find a safe consolidation boundary (end of a user turn)."""
        tokens = 0
        last_user_idx = None
        for i in range(start, len(messages)):
            content = messages[i].get("content", "")
            tokens += len(str(content)) // 3
            if messages[i].get("role") == "user":
                last_user_idx = i
            if tokens >= target_tokens and last_user_idx is not None:
                return last_user_idx
        return last_user_idx

    @staticmethod
    def _format_messages(messages: list[dict[str, Any]]) -> str:
        lines = []
        for msg in messages:
            content = msg.get("content", "")
            if not content:
                continue
            ts = msg.get("timestamp", "?")[:16]
            role = msg.get("role", "?").upper()
            lines.append(f"[{ts}] {role}: {content}")
        return "\n".join(lines)
