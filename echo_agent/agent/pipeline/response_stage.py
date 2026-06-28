"""Response stage — post-processing, session save, and background task scheduling."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.utils.text import strip_thinking

if TYPE_CHECKING:
    from collections.abc import Callable, Coroutine

    from echo_agent.agent.consolidation import ConsolidationWorker
    from echo_agent.config.schema import Config
    from echo_agent.memory.store import MemoryStore
    from echo_agent.models.provider import LLMProvider
    from echo_agent.session.manager import SessionManager


# NOTE: mirror of echo_agent.agent.streaming.ProcessResult; loop._process_event
# bridges between them. Add fields to BOTH or the bridge silently drops them.
@dataclass
class ProcessResult:
    response_text: str = ""
    outbound_sent: bool = False
    degraded_notices: list[str] = field(default_factory=list)


# Channels whose traffic is synthetic (evaluation/benchmark/test harnesses) and
# must never pollute long-term memory. Their session keys look like "eval:...".
_NON_PERSISTING_CHANNELS = frozenset({"eval", "test", "benchmark"})


def _is_ephemeral_session(session_key: str, channel: str) -> bool:
    """True if this traffic should be excluded from consolidation/memory review.

    Eval/test traffic was previously consolidated into MEMORY.md, producing noise
    like 'rejected rm -rf 25 times' that drowned out real user facts.
    """
    if channel and channel.lower() in _NON_PERSISTING_CHANNELS:
        return True
    prefix = session_key.split(":", 1)[0].lower() if session_key else ""
    return prefix in _NON_PERSISTING_CHANNELS


class ResponseStage:
    """Finalizes the response: strips thinking, saves session, triggers background work."""

    def __init__(
        self,
        *,
        config: Config,
        sessions: SessionManager,
        memory: MemoryStore,
        provider: LLMProvider,
        consolidation_worker: ConsolidationWorker,
        default_model: str,
        spawn_fn: Callable[[Any], None],
        clear_memory_snapshot_fn: Callable[[str], Coroutine[Any, Any, None]],
        skill_store: Any = None,
        skill_admission: Any = None,
        working_memories: Any = None,
        prefetcher: Any = None,
    ):
        self._config = config
        self._sessions = sessions
        self._memory = memory
        self._provider = provider
        self._consolidation = consolidation_worker
        self._default_model = default_model
        self._spawn_fn = spawn_fn
        self._clear_memory_snapshot = clear_memory_snapshot_fn
        self._skill_store = skill_store
        self._skill_admission = skill_admission
        self._working_memories = working_memories
        self._prefetcher = prefetcher

    async def finalize(self, ctx: PipelineContext, result: InferenceResult) -> ProcessResult:
        """Post-process inference result, save session, schedule background tasks."""
        event = ctx.event
        session = ctx.session
        response_text = result.response_text

        if response_text:
            response_text = strip_thinking(response_text)
        if ctx.intro_text:
            response_text = f"{ctx.intro_text}\n\n{response_text}" if response_text else ctx.intro_text

        session.add_message("assistant", response_text)
        await self._sessions.save(session)

        # Working-memory write-back: record this turn so the *next* turn's
        # context injection ("## Active Context") is non-empty. Previously
        # WorkingMemory had a reader (context_stage) but no writer, so the
        # injection was always blank. Skip ephemeral eval/test traffic.
        self._update_working_memory(session.key, event, response_text)

        # Flush pending memory embeddings. DURABLE: a dropped flush silently
        # loses embeddings, so pass a zero-arg factory (retry-capable) and tag
        # the tier so it is queued — never dropped — under saturation.
        if self._memory.has_pending_embeds():
            from echo_agent.agent.background import Tier
            self._spawn_fn(lambda: self._memory.flush_pending_embeds(), tier=Tier.DURABLE)

        # Eval/test traffic must never feed long-term memory or memory review —
        # otherwise synthetic benchmark noise accumulates in MEMORY.md.
        ephemeral = _is_ephemeral_session(session.key, event.channel)

        # Schedule consolidation (safe — acquires its own lock). DURABLE: the
        # consolidation commit must not be dropped under load.
        if not ephemeral and hasattr(self._consolidation, '_consolidator'):
            from echo_agent.agent.background import Tier
            consolidator = self._consolidation._consolidator
            if consolidator.should_consolidate(session.message_count, session.last_consolidated):
                await self._consolidation.schedule(
                    session.key,
                    self._spawn_fn,
                    on_complete=self._clear_memory_snapshot,
                    tier=Tier.DURABLE,
                )

        # Background skill/memory reviews.
        # Skill review still requires tool activity (it reviews tool usage patterns).
        # Memory review must run even for pure chat — personal facts are shared in
        # plain conversation without any tool calls, so it does NOT gate on tool count.
        if result.should_review_skills and result.total_tool_calls > 0 and not ephemeral:
            self._spawn_fn(self._background_skill_review(
                ctx.messages, event.session_key, event.channel,
            ))
        if result.should_review_memory and not ephemeral:
            self._spawn_fn(self._background_memory_review(ctx.messages, event.session_key))

        # Finalize streaming
        outbound_sent = False
        if ctx.publish_response and ctx.stream_publisher:
            outbound_sent = await ctx.stream_publisher.finalize(response_text)

        # Reply is now out the door. Prefetch the NEXT turn's retrieval using
        # this turn's query and write it to the per-session cache, so a
        # continuing same-topic conversation hits the cache on its next turn
        # (zero inline retrieval latency). DISCARDABLE: a dropped prefetch is
        # harmless — the next turn simply misses and falls back to inline
        # retrieval — so a bare coroutine (no retry factory) is fine.
        if self._prefetcher is not None and event.text:
            from echo_agent.agent.background import Tier
            self._spawn_fn(
                self._prefetcher.prefetch(
                    event.session_key, event.text, event.sender_id
                ),
                tier=Tier.DISCARDABLE,
            )

        return ProcessResult(
            response_text=response_text or "",
            outbound_sent=outbound_sent,
            degraded_notices=list(result.degraded_notices),
        )

    def _update_working_memory(self, session_key: str, event: Any, response_text: str) -> None:
        """Record the latest exchange into WorkingMemory so the next turn can
        inject it as Active Context. No-op when working memory is unwired or the
        session is ephemeral."""
        if self._working_memories is None:
            return
        if _is_ephemeral_session(session_key, getattr(event, "channel", "")):
            return
        wm = self._working_memories.get(session_key)
        if wm is None:
            return
        try:
            from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType

            user_text = (getattr(event, "text", "") or "").strip()
            if user_text:
                wm.add(MemoryEntry(
                    type=MemoryType.USER, tier=MemoryTier.WORKING,
                    key="user", content=user_text[:500], source_session=session_key,
                ))
            reply = (response_text or "").strip()
            if reply:
                wm.add(MemoryEntry(
                    type=MemoryType.USER, tier=MemoryTier.WORKING,
                    key="assistant", content=reply[:500], source_session=session_key,
                ))
        except Exception as e:
            logger.debug("Working memory update failed: {}", e)

    async def _background_skill_review(
        self, messages: list[dict[str, Any]], session_key: str = "", channel: str = "",
    ) -> None:
        try:
            from echo_agent.skills.reviewer import SkillReviewer
            if self._skill_store is None:
                logger.warning("Skill review skipped: no skill store configured")
                return
            reviewer = SkillReviewer(
                provider=self._provider,
                store=self._skill_store,
                model=self._default_model,
                admission=self._skill_admission,
                session_key=session_key,
                channel=channel,
            )
            actions = await reviewer.review(messages)
            if actions:
                logger.info("Background skill review: {}", "; ".join(actions))
        except Exception as e:
            logger.warning("Background skill review failed: {}", e)

    async def _background_memory_review(self, messages: list[dict[str, Any]], session_key: str) -> None:
        try:
            from echo_agent.memory.reviewer import MemoryReviewer
            reviewer = MemoryReviewer(
                provider=self._provider,
                store=self._memory,
                model=self._default_model,
                session_key=session_key,
            )
            actions = await reviewer.review(messages)
            if actions:
                logger.info("Background memory review: {}", "; ".join(actions))
                await self._clear_memory_snapshot(session_key)
        except Exception as e:
            logger.warning("Background memory review failed: {}", e)
