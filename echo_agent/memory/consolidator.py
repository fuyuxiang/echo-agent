"""Memory consolidator — summarizes conversation chunks into long-term memory."""

from __future__ import annotations

import json
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.memory.render import render_memory_md
from echo_agent.memory.store import MemoryStore

_EXTRACT_FACTS_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "save_facts",
            "description": "Record durable facts distilled from an episode summary.",
            "parameters": {
                "type": "object",
                "properties": {
                    "facts": {
                        "type": "array",
                        "description": "Durable facts; empty when nothing is worth keeping.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": {"type": "string", "enum": ["user", "environment"]},
                                "key": {"type": "string"},
                                "content": {"type": "string"},
                                "importance": {"type": "number"},
                            },
                            "required": ["key", "content"],
                        },
                    },
                },
                "required": ["facts"],
            },
        },
    }
]


class MemoryConsolidator:
    """Consolidates conversation history into the structured store; MEMORY.md is
    a deterministic rendered view of ACTIVE entries (no LLM rewrite)."""

    _MAX_ROUNDS = 3
    _EPISODE_RETENTION_DAYS = 90

    def __init__(
        self,
        memory_store: MemoryStore,
        llm_call: Callable[..., Awaitable[Any]],
        context_window_tokens: int = 65536,
        consolidation_threshold: int = 50,
        episode_retention_days: int | None = None,
    ):
        self.store = memory_store
        self._llm_call = llm_call
        self.context_window_tokens = context_window_tokens
        self._consolidation_threshold = consolidation_threshold
        self._episode_retention_days = (
            episode_retention_days if episode_retention_days is not None else self._EPISODE_RETENTION_DAYS
        )
        self._episodic_manager = None  # set via set_episodic_manager()
        self._semantic_manager = None
        self._forgetting_curve = None
        self._contradiction_detector = None
        self._archival_manager = None
        self._embed_fn = None
        self._auto_resolve_contradictions = False
        self._reflection_engine = None

    def set_episodic_manager(self, mgr):
        self._episodic_manager = mgr

    def set_semantic_manager(self, mgr):
        self._semantic_manager = mgr

    def set_forgetting_curve(self, curve):
        self._forgetting_curve = curve

    def set_contradiction_detector(self, detector):
        self._contradiction_detector = detector

    def set_archival_manager(self, mgr):
        self._archival_manager = mgr

    def set_embed_fn(self, fn):
        self._embed_fn = fn

    def set_auto_resolve_contradictions(self, enabled: bool):
        self._auto_resolve_contradictions = enabled

    def set_reflection(self, engine):
        self._reflection_engine = engine

    async def consolidate_chunk(
        self, messages: list[dict[str, Any]], memory_scope: str = ""
    ) -> bool:
        if not messages:
            return True

        # R3: consolidate_chunk no longer runs an LLM rewrite chain over
        # MEMORY.md / HISTORY.md. The structured store is the single
        # source of truth; MEMORY.md is later re-rendered deterministically by
        # sleep_consolidate after promote. This method now only reports whether
        # the chunk carries substantive content worth turning into an episode —
        # the bool return still gates episode creation (sleep_consolidate) and
        # the worker's boundary advance (consolidation.py).
        try:
            formatted = self._format_messages(messages)
            if not formatted.strip():
                # Nothing but empty/tool-noise turns — no episode to create.
                return False
            return True
        except Exception as e:
            logger.error("Chunk consolidation signal failed: {}", e)
            return False

    async def sleep_consolidate(
        self, session_key: str, messages: list[dict[str, Any]],
        *, chunk_already_consolidated: bool = False, memory_scope: str = "",
        range_start: int = 0,
    ) -> dict[str, int]:
        """Sleep-time consolidation pipeline:
        1. Create episode from messages
        2. Extract semantic facts and promote
        3. Run heuristic contradiction detection on promoted entries
        4. Run forgetting/archival pass
        Returns stats dict.
        """
        stats = {"episodes": 0, "promoted": 0, "contradictions": 0, "resolved": 0, "archived": 0, "forgotten": 0,
                 "fact_extract_errors": 0, "reflection_errors": 0}
        promoted: list = []

        # Step 1: Create episode
        if self._episodic_manager and messages:
            if chunk_already_consolidated:
                summary_result = True
            else:
                summary_result = await self.consolidate_chunk(messages, memory_scope)
            if summary_result:
                summary_text = await self._generate_episode_summary(messages)
                episode = await self._episodic_manager.create_episode(
                    session_key=session_key,
                    messages=messages,
                    summary=summary_text,
                    message_range=(range_start, range_start + len(messages)),
                )
                stats["episodes"] = 1

                # Step 2: Promote to semantic
                if self._semantic_manager:
                    try:
                        response = await self._llm_call(
                            messages=[
                                {"role": "system", "content": "Extract durable facts from this episode summary and call save_facts. Return an empty list when nothing is worth keeping."},
                                {"role": "user", "content": episode.summary},
                            ],
                            tools=_EXTRACT_FACTS_TOOL,
                            tool_choice={"type": "function", "function": {"name": "save_facts"}},
                        )
                        facts = self._parse_extracted_facts(response)
                        if facts:
                            promoted = await self._semantic_manager.promote_from_episodic(episode, facts, memory_scope=memory_scope)
                            stats["promoted"] = len(promoted)
                    except Exception as e:
                        # Single-step degradation: keep the rest of the sleep
                        # pipeline running, but record the failure in stats so it
                        # is observable (test/monitoring) rather than silent.
                        stats["fact_extract_errors"] += 1
                        logger.warning("Fact extraction failed: {}", e)

        # Step 3: Run contradiction detection on newly promoted entries.
        # When auto-resolution is enabled, same-key conflicts are resolved by
        # reusing the very Contradiction object just stored — no second detect,
        # no second uuid, no second row. This prevents the "ghost" unresolved
        # record that a separate re-detect pass would leave in get_unresolved.
        if self._contradiction_detector and promoted:
            try:
                if memory_scope:
                    all_entries = self.store.list_all(session_key=memory_scope)
                else:
                    all_entries = list(self.store._entries.values())
                entry_map = {e.id: e for e in all_entries}
                for new_entry in promoted:
                    # 排除 superseded 旧版本:list_all 默认 audience=None 不过滤生命周期,
                    # 同 key 改口后的 superseded 兄弟会混入比较集合,被启发式判"矛盾",
                    # 进而把新 active 条目误标 unresolved → 从召回/快照静默剔除。与写时
                    # 扫描 _run_contradiction_scan 的 `not e.is_superseded` 口径统一。
                    others = [e for e in all_entries if e.id != new_entry.id and not e.is_superseded]
                    contradictions = await self._contradiction_detector.check(
                        new_entry, others, llm_call=self._llm_call, embed_fn=self._embed_fn,
                    )
                    for c in contradictions:
                        await self._contradiction_detector.store_contradiction(c)
                        stats["contradictions"] += 1
                        if self._auto_resolve_contradictions and await self._auto_resolve_same_key(c, entry_map):
                            stats["resolved"] += 1
            except Exception as e:
                logger.warning("Contradiction detection failed: {}", e)

        # Step 4: Run forgetting pass
        if self._forgetting_curve:
            if memory_scope:
                all_entries = self.store.list_all(session_key=memory_scope)
            else:
                all_entries = list(self.store._entries.values())
            to_archive, to_forget = await self._forgetting_curve.run_decay_pass(all_entries)
            if to_archive and self._archival_manager:
                stats["archived"] = await self._archival_manager.archive(to_archive)
            if to_forget and self._archival_manager:
                stats["forgotten"] = await self._archival_manager.delete_forgotten(to_forget)

        # Step 5: Purge expired episodes — the episodes table is otherwise
        # append-only and grows without bound.
        if self._episodic_manager and self._episode_retention_days > 0:
            try:
                from datetime import datetime, timedelta
                cutoff = (datetime.now() - timedelta(days=self._episode_retention_days)).isoformat()
                await self._episodic_manager._storage.execute_sql(
                    "DELETE FROM memory_episodes WHERE created_at < ?", (cutoff,),
                )
            except Exception as e:
                logger.debug("Episode retention purge failed: {}", e)

        # Step 6: Reflection — distill + active conflict resolution
        if self._reflection_engine:
            try:
                reflection_stats = await self._reflection_engine.run(memory_scope=memory_scope)
                stats.update(reflection_stats)
            except Exception as e:
                # Single-step degradation: don't abort the pipeline, but record
                # the reflection failure in stats so it is observable.
                stats["reflection_errors"] += 1
                logger.warning("Reflection engine failed: {}", e)

        # Step 7: Deterministically re-render MEMORY.md from the scope's current
        # ACTIVE set. This lands AFTER every lifecycle mutation of this run
        # (Step 2 promote/supersede, Step 3 auto-resolve supersede, Step 4
        # forgetting/archival, Step 6 reflection) rather than inside Step 2's
        # `if facts:` block — so the snapshot never keeps entries that a later
        # step superseded/archived, and runs that only archive or only re-adjudicate
        # (no new facts) still refresh the file. render_memory_md filters
        # superseded/ARCHIVAL, so MEMORY.md stays a pure, idempotent snapshot of
        # the store (human-facing export; does NOT enter the prompt), keeping the
        # store as the single source of truth. Unconditional: not gated on facts.
        try:
            visible = self.store.list_all(session_key=memory_scope)
            rendered = render_memory_md(visible)
            self.store.write_long_term(memory_scope, rendered)
        except Exception as re:
            logger.warning("MEMORY.md re-render failed: {}", re)

        logger.info("Sleep consolidation complete: {}", stats)
        return stats

    async def _auto_resolve_same_key(self, c, entry_map: dict) -> bool:
        """Resolve a single already-stored contradiction iff it is a same-key
        content conflict. Provenance priority adjudicates first; newest-wins
        only breaks ties within the same priority. Legacy-source parties are
        never auto-resolved.

        Note that user_stated is not sticky: a same-key replace without an
        explicit user statement downgrades the entry to model_inferred (the
        deliberate conservative default), so adjudication here reads the
        entry's *current* source, not its historical maximum.

        Reuses the Contradiction ``c`` that Step 3 already detected and stored:
        it is resolved in place (same row, same uuid), so get_unresolved never
        retains a ghost copy.

        Deliberately narrow: LLM/temporal/cross-key contradictions are left
        unresolved for human review. Returns True when c was resolved.
        """
        if not self._contradiction_detector:
            return False
        a = entry_map.get(c.memory_id_a)
        b = entry_map.get(c.memory_id_b)
        if a is None or b is None:
            return False
        # Same full key + differing content is the only auto-resolvable case.
        if not a.key or a.key != b.key:
            return False
        # 同 key 不同 scope 是合法的不同主体事实(写入侧 _find_conflict 也按
        # _same_scope 不 merge),不得判矛盾/跨 scope supersede。
        if not self.store._same_scope(a, b):
            return False
        if a.content.strip() == b.content.strip():
            return False
        if a.is_superseded or b.is_superseded:
            return False
        from echo_agent.memory.types import source_priority
        pa, pb = source_priority(a.source), source_priority(b.source)
        if pa == 0 or pb == 0:
            # legacy/unknown provenance: no basis for automatic adjudication —
            # leave it for human/model review via resolve_contradiction.
            return False
        if pa != pb:
            winner = a if pa > pb else b
        else:
            winner, _loser = self._newest_wins(a, b)
        resolution = "a_wins" if winner.id == c.memory_id_a else "b_wins"
        # resolve 现返回 bool:supersede 失败时矛盾行保持 unresolved,本次视为未消解,
        # 下轮巩固/反思会重试。
        return await self._contradiction_detector.resolve(
            c.id, resolution, winner_id=winner.id
        )

    @staticmethod
    def _newest_wins(a, b):
        def _ts(e):
            return e.updated_at or e.created_at or ""
        return (a, b) if _ts(a) >= _ts(b) else (b, a)

    def should_consolidate(self, session_message_count: int, last_consolidated: int) -> bool:
        unconsolidated = session_message_count - last_consolidated
        return unconsolidated >= self._consolidation_threshold

    @staticmethod
    def _parse_extracted_facts(response: Any) -> list:
        """Extract the facts list from a save_facts tool call.

        Replaces the old free-text ``json.loads`` + ``except: pass`` that
        silently dropped every malformed extraction. Parsing problems are now
        logged, not swallowed; a missing tool call yields an empty list."""
        tool_calls = getattr(response, "tool_calls", None) or []
        if not tool_calls:
            logger.debug("Fact extraction: model returned no save_facts call")
            return []
        args = tool_calls[0].arguments
        if isinstance(args, str):
            try:
                args = json.loads(args)
            except json.JSONDecodeError as e:
                logger.warning("Fact extraction: could not parse tool arguments: {}", e)
                return []
        facts = args.get("facts", []) if isinstance(args, dict) else []
        if not isinstance(facts, list):
            logger.warning("Fact extraction: 'facts' was not a list (got {})", type(facts).__name__)
            return []
        return facts

    async def _generate_episode_summary(self, messages: list[dict[str, Any]]) -> str:
        """Generate a concise summary of conversation messages for episode storage."""
        formatted = self._format_messages(messages)
        if not formatted:
            return "conversation episode"
        try:
            response = await self._llm_call(
                messages=[
                    {"role": "system", "content": "Summarize this conversation in 2-3 sentences. Focus on what was discussed, decided, or accomplished."},
                    {"role": "user", "content": formatted[:3000]},
                ],
            )
            if response.content and response.content.strip():
                return response.content.strip()[:500]
        except Exception as e:
            logger.warning("Episode summary generation failed: {}", e)
        # Fallback: use first user message as summary
        for msg in messages:
            if msg.get("role") == "user" and msg.get("content"):
                return str(msg["content"])[:200]
        return "conversation episode"

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
            if isinstance(content, list):
                parts = []
                for block in content:
                    if isinstance(block, dict):
                        if block.get("type") == "text":
                            parts.append(block.get("text", ""))
                        elif block.get("type") in ("image_url", "image"):
                            parts.append("[image]")
                    else:
                        parts.append(str(block))
                content = " ".join(parts)
                if not content.strip():
                    continue
            ts = str(msg.get("timestamp", "?"))[:16]
            role = msg.get("role", "?").upper()
            lines.append(f"[{ts}] {role}: {content}")
        return "\n".join(lines)
