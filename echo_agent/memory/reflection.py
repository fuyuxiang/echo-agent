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

_VALID_VERDICTS = frozenset(["a_wins", "b_wins", "ambiguous", "not_contradictory"])

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

_ADJUDICATE_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "adjudicate",
            "description": "Adjudicate whether two memories conflict and which one holds.",
            "parameters": {
                "type": "object",
                "properties": {
                    "verdict": {
                        "type": "string",
                        "enum": ["a_wins", "b_wins", "ambiguous", "not_contradictory"],
                    },
                    "explanation": {"type": "string"},
                },
                "required": ["verdict"],
            },
        },
    }
]

_MIN_GROUP_SIZE = 3


class ReflectionEngine:
    """LLM-backed distillation over prefix-grouped semantic memories."""

    NEEDS_CONFIRMATION_TAG = "needs_user_confirmation"

    def __init__(
        self,
        store: MemoryStore,
        llm_call: Callable[..., Awaitable[Any]],
        contradiction_detector: Any = None,
    ):
        self._store = store
        self._llm_call = llm_call
        self._detector = contradiction_detector

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

    # ── Conflict resolution ──────────────────────────────────────────────────

    async def _conflict_pairs(self) -> list[tuple[MemoryEntry, MemoryEntry]]:
        """Pair up suspected_conflict entries for adjudication.

        Prefers the detector's authoritative (memory_id_a, memory_id_b) rows —
        those record which two entries actually conflict. Falls back to
        key-prefix grouping only when no detector is wired. The prefix fallback
        can mis-pair (two entries under the same prefix that aren't the actual
        conflicting pair), so the exact detector pairing is used whenever
        available.
        """
        flagged = [
            e for e in self._store.list_all()
            if MemoryStore.SUSPECTED_CONFLICT_TAG in e.tags
            and not e.is_superseded
            and self.NEEDS_CONFIRMATION_TAG not in e.tags
        ]
        # Ensure flagged entries are persisted (they may have been injected in
        # memory only, e.g. by the provenance guard's blocked path).
        for e in flagged:
            if e.id not in self._store._dirty_ids:
                self._store._dirty_ids.add(e.id)
        if flagged:
            types_to_save = {e.type for e in flagged}
            for t in types_to_save:
                self._store._save_type(t)
        flagged_by_id = {e.id: e for e in flagged}

        # Preferred path: use the detector's exact conflict pairs, restricted to
        # currently-flagged, non-superseded entries. De-dup unordered pairs.
        if self._detector is not None:
            try:
                unresolved = await self._detector.get_unresolved(limit=10000)
                pairs: list[tuple[MemoryEntry, MemoryEntry]] = []
                seen: set[frozenset[str]] = set()
                for c in unresolved:
                    a = flagged_by_id.get(c.memory_id_a)
                    b = flagged_by_id.get(c.memory_id_b)
                    if a is None or b is None or a.id == b.id:
                        continue
                    key = frozenset((a.id, b.id))
                    if key in seen:
                        continue
                    seen.add(key)
                    pairs.append((a, b))
                if pairs:
                    return pairs
            except Exception as e:
                logger.warning("Reflection: detector pairing failed, falling back: {}", e)

        # Fallback: key-prefix grouping (oldest two per prefix).
        by_prefix: dict[str, list[MemoryEntry]] = defaultdict(list)
        for e in flagged:
            prefix = e.key.split(":")[0] if ":" in e.key else e.key
            by_prefix[prefix].append(e)
        pairs = []
        for group in by_prefix.values():
            if len(group) >= 2:
                group.sort(key=lambda e: e.updated_at or "")
                pairs.append((group[0], group[1]))
        return pairs

    @staticmethod
    def _priority_allows_supersede(winner: MemoryEntry, loser: MemoryEntry) -> bool:
        """Whether ``winner`` may supersede ``loser`` under the provenance floor.

        Same contract as consolidator._auto_resolve_same_key:
        - either side legacy/unknown (priority 0) → no basis, block;
        - winner strictly lower priority than loser → block (a lower-provenance
          entry must never bury a higher one);
        - winner priority >= loser → allow (equal priority lets the LLM's
          content judgment stand; higher obviously wins).
        """
        from echo_agent.memory.types import source_priority
        pw, pl = source_priority(winner.source), source_priority(loser.source)
        if pw == 0 or pl == 0:
            return False
        return pw >= pl

    def _clear_conflict_tags(self, *entries: MemoryEntry) -> None:
        for e in entries:
            current = self._store.get(e.id)
            if current is None:
                continue
            tags = [t for t in current.tags if t != MemoryStore.SUSPECTED_CONFLICT_TAG]
            self._store.update(e.id, tags=tags)

    def _defer_to_user(self, *entries: MemoryEntry) -> None:
        for e in entries:
            current = self._store.get(e.id)
            if current is None:
                continue
            if self.NEEDS_CONFIRMATION_TAG not in current.tags:
                self._store.update(e.id, tags=[*current.tags, self.NEEDS_CONFIRMATION_TAG])

    async def _resolve_detector_rows(
        self, a: MemoryEntry, b: MemoryEntry, resolution: str, winner_id: str | None,
    ) -> None:
        """Close ALL open detector rows for this (a, b) pair — handles I1 dedup."""
        if not self._detector:
            return
        try:
            unresolved = await self._detector.get_unresolved(limit=10000)
            for c in unresolved:
                if (
                    (c.memory_id_a == a.id and c.memory_id_b == b.id)
                    or (c.memory_id_a == b.id and c.memory_id_b == a.id)
                ):
                    await self._detector.resolve(c.id, resolution, winner_id)
        except Exception as e:
            logger.warning("Reflection: failed to resolve detector rows: {}", e)

    async def _ask_adjudicate(self, a: MemoryEntry, b: MemoryEntry) -> str:
        """Ask LLM to adjudicate; returns validated verdict or 'ambiguous'."""
        response = await self._llm_call(
            messages=[
                {"role": "system", "content": (
                    "You adjudicate whether two memory entries truly conflict. "
                    "Always call 'adjudicate' with your verdict."
                )},
                {"role": "user", "content": (
                    f"Memory A (key={a.key}, source={a.source}): {a.content}\n"
                    f"Memory B (key={b.key}, source={b.source}): {b.content}\n\n"
                    "Which holds? Call adjudicate."
                )},
            ],
            tools=_ADJUDICATE_TOOL,
            tool_choice={"type": "function", "function": {"name": "adjudicate"}},
        )
        tool_calls = getattr(response, "tool_calls", None) or []
        if not tool_calls:
            return "ambiguous"
        args = tool_calls[0].arguments
        if isinstance(args, str):
            args = json.loads(args)
        verdict = args.get("verdict", "ambiguous")
        # Whitelist validation — reject hallucinated values
        if verdict not in _VALID_VERDICTS:
            return "ambiguous"
        return verdict

    async def resolve_conflicts(self, max_pairs: int = 3) -> dict:
        """LLM-adjudicate suspected conflicts. Whitelist-validated verdicts:
        clear -> supersede; ambiguous/invalid -> defer to user; not_contradictory
        -> clear tags."""
        stats = {"resolved": 0, "deferred": 0, "dismissed": 0}
        try:
            pairs = await self._conflict_pairs()
        except Exception as e:
            logger.warning("Reflection resolve: pairing failed: {}", e)
            return stats
        for a, b in pairs[:max_pairs]:
            try:
                verdict = await self._ask_adjudicate(a, b)
            except Exception as e:
                logger.warning("Reflection adjudication failed for {}/{}: {}", a.id, b.id, e)
                continue
            # Source-priority floor: the LLM verdict may not override the
            # provenance hierarchy. Mirrors consolidator._auto_resolve_same_key
            # so both adjudication paths honor one contract — legacy/unknown
            # never auto-resolves, and a lower-provenance entry never supersedes
            # a higher one (e.g. model_inferred must not bury user_stated).
            # Violations degrade to "defer to user" instead of supersede.
            if verdict in ("a_wins", "b_wins"):
                winner, loser = (a, b) if verdict == "a_wins" else (b, a)
                if not self._priority_allows_supersede(winner, loser):
                    self._defer_to_user(a, b)
                    stats["deferred"] += 1
                    continue
            if verdict == "a_wins":
                self._store.mark_superseded(b.id, a.id)
                self._clear_conflict_tags(a, b)
                await self._resolve_detector_rows(a, b, "a_wins", a.id)
                stats["resolved"] += 1
            elif verdict == "b_wins":
                self._store.mark_superseded(a.id, b.id)
                self._clear_conflict_tags(a, b)
                await self._resolve_detector_rows(a, b, "b_wins", b.id)
                stats["resolved"] += 1
            elif verdict == "not_contradictory":
                self._clear_conflict_tags(a, b)
                await self._resolve_detector_rows(a, b, "not_contradictory", None)
                stats["dismissed"] += 1
            else:  # ambiguous or whitelist-rejected output
                self._defer_to_user(a, b)
                stats["deferred"] += 1
        if any(stats.values()):
            logger.info("Reflection conflict resolution: {}", stats)
        return stats

    async def run(self) -> dict:
        """Run full reflection pass: distill + resolve conflicts."""
        result: dict[str, Any] = {}
        try:
            distilled = await self.distill()
            result["distilled"] = distilled
        except Exception as e:
            logger.warning("Reflection distill phase failed: {}", e)
            result["distilled"] = 0
        try:
            conflict_stats = await self.resolve_conflicts()
            result.update(conflict_stats)
        except Exception as e:
            logger.warning("Reflection resolve phase failed: {}", e)
        return result
