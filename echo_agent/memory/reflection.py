"""Reflection engine — sleep-time distillation and active conflict resolution.

Runs as Step 6 of sleep consolidation (see consolidator.sleep_consolidate).
Distillation groups semantic entries by key prefix and asks the LLM whether a
more abstract rule can be induced; the rule is stored as a NEW entry
(source="consolidated", tag "distilled") and the concrete originals are kept
(add-only; natural forgetting handles them). Failures never propagate — the
whole engine is best-effort background work."""

from __future__ import annotations

import json
from collections import defaultdict
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryTier

_DISTILL_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "save_distilled",
            "description": "Report whether the entries can be distilled into one abstract rule.",
            "parameters": {
                "type": "object",
                "properties": {
                    "distill": {"type": "boolean",
                                "description": "True only if a genuinely more abstract, durable rule exists."},
                    "key": {"type": "string", "description": "Key for the rule (use '<prefix>:general')."},
                    "content": {"type": "string", "description": "The abstract rule."},
                    "importance": {"type": "number", "description": "0.0-1.0"},
                },
                "required": ["distill"],
            },
        },
    }
]

_MIN_GROUP_SIZE = 3


class ReflectionEngine:
    """LLM-backed distillation over prefix-grouped semantic memories."""

    def __init__(self, store: MemoryStore, llm_call: Callable[..., Awaitable[Any]]):
        self._store = store
        self._llm_call = llm_call

    def _prefix_groups(self) -> dict[str, list[MemoryEntry]]:
        groups: dict[str, list[MemoryEntry]] = defaultdict(list)
        for entry in self._store.list_all():
            if entry.is_superseded or entry.tier == MemoryTier.ARCHIVAL:
                continue
            if ":" not in entry.key:
                continue
            groups[entry.key.split(":")[0]].append(entry)
        return groups

    async def distill(self, max_groups: int = 2) -> int:
        """Induce abstract rules from concrete same-prefix facts. Add-only."""
        created = 0
        processed = 0
        try:
            groups = self._prefix_groups()
        except Exception as e:
            logger.warning("Reflection distill: grouping failed: {}", e)
            return 0
        for prefix, entries in sorted(groups.items(), key=lambda kv: -len(kv[1])):
            if processed >= max_groups:
                break
            if len(entries) < _MIN_GROUP_SIZE:
                continue
            general_key = f"{prefix}:general"
            if any(e.key == general_key for e in entries):
                continue
            processed += 1
            try:
                rule = await self._ask_distill(prefix, entries, general_key)
                if rule is not None:
                    self._store.add(rule)
                    created += 1
            except Exception as e:
                logger.warning("Reflection distill failed for prefix '{}': {}", prefix, e)
        if created:
            logger.info("Reflection distilled {} new rule(s)", created)
        return created

    async def _ask_distill(
        self, prefix: str, entries: list[MemoryEntry], general_key: str,
    ) -> MemoryEntry | None:
        listing = "\n".join(f"- {e.key}: {e.content}" for e in entries[:10])
        response = await self._llm_call(
            messages=[
                {"role": "system", "content": (
                    "You review a group of related memory facts and decide whether "
                    "ONE genuinely more abstract, durable rule can be induced. Be "
                    "conservative: prefer distill=false unless the pattern is clear. "
                    "Always call save_distilled."
                )},
                {"role": "user", "content": (
                    f"Facts under prefix '{prefix}':\n{listing}\n\n"
                    f"If distillable, use key '{general_key}'."
                )},
            ],
            tools=_DISTILL_TOOL,
            tool_choice={"type": "function", "function": {"name": "save_distilled"}},
        )
        tool_calls = getattr(response, "tool_calls", None) or []
        if not tool_calls:
            return None
        args = tool_calls[0].arguments
        if isinstance(args, str):
            args = json.loads(args)
        if not args.get("distill") or not args.get("content"):
            return None
        try:
            importance = min(1.0, max(0.0, float(args.get("importance", 0.6))))
        except (TypeError, ValueError):
            importance = 0.6
        sample = entries[0]
        return MemoryEntry(
            type=sample.type,
            key=general_key,
            content=str(args["content"]),
            tags=["distilled"],
            importance=importance,
            source="consolidated",
            source_session=sample.source_session,
        )
