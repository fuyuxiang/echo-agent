"""Response stage — post-processing, session save, and background task scheduling."""

from __future__ import annotations

from dataclasses import dataclass
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


@dataclass
class ProcessResult:
    response_text: str = ""
    outbound_sent: bool = False


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
    ):
        self._config = config
        self._sessions = sessions
        self._memory = memory
        self._provider = provider
        self._consolidation = consolidation_worker
        self._default_model = default_model
        self._spawn_fn = spawn_fn
        self._clear_memory_snapshot = clear_memory_snapshot_fn

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

        # Flush pending memory embeddings
        if self._memory.has_pending_embeds():
            self._spawn_fn(self._memory.flush_pending_embeds())

        # Schedule consolidation (safe — acquires its own lock)
        from echo_agent.memory.consolidator import MemoryConsolidator
        if hasattr(self._consolidation, '_consolidator'):
            consolidator = self._consolidation._consolidator
            if consolidator.should_consolidate(session.message_count, session.last_consolidated):
                await self._consolidation.schedule(
                    session.key,
                    self._spawn_fn,
                    on_complete=self._clear_memory_snapshot,
                )

        # Background skill/memory reviews
        if result.should_review_skills and result.total_tool_calls > 0:
            self._spawn_fn(self._background_skill_review(ctx.messages))
        if result.should_review_memory and result.total_tool_calls > 0:
            self._spawn_fn(self._background_memory_review(ctx.messages, event.session_key))

        # Finalize streaming
        outbound_sent = False
        if ctx.publish_response and ctx.stream_publisher:
            outbound_sent = await ctx.stream_publisher.finalize(response_text)

        return ProcessResult(response_text=response_text or "", outbound_sent=outbound_sent)

    async def _background_skill_review(self, messages: list[dict[str, Any]]) -> None:
        try:
            from echo_agent.skills.reviewer import SkillReviewer
            reviewer = SkillReviewer(
                provider=self._provider,
                model=self._default_model,
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
