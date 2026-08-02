"""Response stage — post-processing, session save, and background task scheduling."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.utils.text import strip_thinking

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Coroutine

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
    task_incomplete: bool = False


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
        scope_version_fn: "Callable[[str], int] | None" = None,
        invalidate_memory_caches_fn: "Callable[[str, bool], Awaitable[None]] | None" = None,
        memory_enabled: bool = True,
        memory_service: Any = None,
    ):
        self._config = config
        # memory.enabled 总开关:关闭时不 flush、不调度 consolidation、不派 Reviewer、
        # 不写回 working memory——与 Task 9 的工具/注入门控配套,避免半开状态。
        self._memory_enabled = memory_enabled
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
        self._scope_version_fn = scope_version_fn
        self._invalidate_memory_caches_fn = invalidate_memory_caches_fn
        # R1 Task8:后台 memory review 用注入的 loop 单例 service,不再每次 new。
        self._memory_service = memory_service
        # R4 Task6 Important2:进程内 per-session "review 进行中" 去重集合。
        # 清零挪到成功回调后,触发条件是 ">= 阈值" 且计数每轮持久化,因此在后台
        # review 成功落 0 之前每一轮 turn 都会重新判定 ≥ 阈值。若无去重,每轮都会
        # 派发一个新 review(昂贵 LLM + 可能重复写)。dispatch 前置标记、review
        # 结束(成功或最终失败)时在 _background_memory_review 的 finally 里清除。
        # ResponseStage 是 loop 内单例(见 loop.py 唯一构造点),该集合天然进程内
        # 唯一;派发判定与 add 都发生在 finalize 内、持该 turn 的 session 锁下,turn
        # 之间被 session 锁串行化,故对同一 session 的 check-then-add 无并发竞态。
        self._memory_review_inflight: set[str] = set()

    async def finalize(self, ctx: PipelineContext, result: InferenceResult) -> ProcessResult:
        """Post-process inference result, save session, schedule background tasks."""
        event = ctx.event
        session = ctx.session
        response_text = result.response_text

        if response_text:
            response_text = strip_thinking(response_text)
        if ctx.intro_text:
            response_text = f"{ctx.intro_text}\n\n{response_text}" if response_text else ctx.intro_text

        # Converge the final user-facing text BEFORE persisting or streaming:
        # degraded notices and the English-filler→Chinese-fallback substitution
        # must land in the session history, the stream, and the outbound
        # publish identically. Converging later (the old loop-level gate) left
        # an English filler in history while the user received the Chinese
        # notice — the next turn's model context diverged from what the user saw.
        # Inspection rounds keep empty text empty ("no news, stay silent").
        if ctx.publish_response:
            from echo_agent.agent.degraded_notice import converge_response_text
            is_inspection = bool(event.metadata.get("_inspection"))
            response_text = converge_response_text(
                response_text,
                list(result.degraded_notices),
                substitute_empty=not is_inspection,
            )

        session.add_message("assistant", response_text)
        await self._sessions.save(session)

        # Working-memory write-back: record this turn so the *next* turn's
        # context injection ("## Active Context") is non-empty. Previously
        # WorkingMemory had a reader (context_stage) but no writer, so the
        # injection was always blank. Skip ephemeral eval/test traffic.
        if self._memory_enabled:
            self._update_working_memory(session.key, event, response_text)

        # Flush pending memory embeddings. DURABLE: a dropped flush silently
        # loses embeddings, so pass a zero-arg factory (retry-capable) and tag
        # the tier so it is queued — never dropped — under saturation.
        if self._memory_enabled and self._memory.has_pending_embeds():
            from echo_agent.agent.background import Tier
            self._spawn_fn(lambda: self._memory.flush_pending_embeds(), tier=Tier.DURABLE)

        # Eval/test traffic must never feed long-term memory or memory review —
        # otherwise synthetic benchmark noise accumulates in MEMORY.md.
        ephemeral = _is_ephemeral_session(session.key, event.channel)

        # Schedule consolidation (safe — acquires its own lock). DURABLE: the
        # consolidation commit must not be dropped under load.
        if self._memory_enabled and not ephemeral and hasattr(self._consolidation, '_consolidator'):
            from echo_agent.agent.background import Tier
            consolidator = self._consolidation._consolidator
            if consolidator.should_consolidate(session.message_count, session.last_consolidated):
                scope = event.memory_scope
                async def _on_consolidated(session_key: str) -> None:
                    await self._clear_memory_snapshot(session_key)
                    # consolidation 重写了该 scope 的长期记忆分片,bump 版本使
                    # 共享该 scope 但挂在其他 session_key 上的快照/检索缓存读时失效,
                    # 而非仅清本 consolidating session 的快照。
                    if self._invalidate_memory_caches_fn is not None and scope:
                        await self._invalidate_memory_caches_fn(scope, False)
                await self._consolidation.schedule(
                    session.key,
                    self._spawn_fn,
                    on_complete=_on_consolidated,
                    tier=Tier.DURABLE,
                    memory_scope=event.memory_scope,
                )

        # Background skill/memory reviews.
        # Skill review still requires tool activity (it reviews tool usage patterns).
        # Memory review must run even for pure chat — personal facts are shared in
        # plain conversation without any tool calls, so it does NOT gate on tool count.
        if result.should_review_skills and result.total_tool_calls > 0 and not ephemeral:
            self._spawn_fn(self._background_skill_review(
                ctx.messages, event.session_key, event.channel,
            ))
        if self._memory_enabled and result.should_review_memory and not ephemeral:
            # DURABLE: a dropped/failed memory review must not silently lose this
            # batch's facts. Pass a zero-arg factory (retry-capable) so the
            # scheduler can re-run it — a bare coroutine cannot be re-awaited.
            # The review also owns clearing the nudge counters, but only AFTER it
            # succeeds: clearing at the trigger point (inference_stage) meant a
            # failed review left the counters at 0, so this batch would never be
            # reviewed again. On success the review resets + persists; on failure
            # it re-raises so the counters stay put and next turn re-triggers.
            #
            # In-flight dedupe (R4 Task6 Important2): the trigger is now level
            # (">= 阈值") and the counter is cleared only on the review's success
            # callback, so every turn between the trigger and that callback would
            # re-dispatch a fresh review. Guard on a per-session in-flight flag:
            # skip dispatch while a review for this session is still running.
            # Both the check and the add happen here under the turn's session
            # lock, so concurrent turns for the same session cannot both pass.
            if session.key not in self._memory_review_inflight:
                self._memory_review_inflight.add(session.key)
                from echo_agent.agent.background import Tier
                self._spawn_fn(
                    lambda: self._background_memory_review(
                        ctx.messages, event.session_key, event.memory_scope,
                    ),
                    tier=Tier.DURABLE,
                )

        # Finalize streaming
        outbound_sent = False
        if ctx.publish_response and ctx.stream_publisher:
            # finalize now returns a DeliveryResult. Only a real delivery (or
            # accepted) receipt suppresses the plain final republish; NO_HANDLER
            # (streaming path not taken) and FAILED both fall through to it.
            receipt = await ctx.stream_publisher.finalize(response_text)
            outbound_sent = receipt.ok

        # Reply is now out the door. Prefetch the NEXT turn's retrieval using
        # this turn's query and write it to the per-session cache, so a
        # continuing same-topic conversation hits the cache on its next turn
        # (zero inline retrieval latency). DISCARDABLE: a dropped prefetch is
        # harmless — the next turn simply misses and falls back to inline
        # retrieval — so a bare coroutine (no retry factory) is fine.
        if self._memory_enabled and self._prefetcher is not None and event.text:
            from echo_agent.agent.background import Tier
            self._spawn_fn(
                self._prefetcher.prefetch(
                    event.session_key, event.text, event.sender_id, event.memory_scope,
                    # 预取用当前 scope 版本盖戳:写后版本一致仍可命中,该 scope
                    # 被写 bump 版本后版本不符即自然失效(缓存条目不会陈旧命中)。
                    # 无注入函数时回退 0(未发生写时读侧 cur_ver 亦为 0 视为命中)。
                    scope_version=(
                        self._scope_version_fn(event.memory_scope)
                        if self._scope_version_fn is not None else 0
                    ),
                    channel=event.channel,
                ),
                tier=Tier.DISCARDABLE,
            )

        return ProcessResult(
            response_text=response_text or "",
            outbound_sent=outbound_sent,
            degraded_notices=list(result.degraded_notices),
            task_incomplete=result.task_incomplete,
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

            # source_session 用 owner-aware 的 memory_scope(跨通道归一/群聊隔离);
            # working_memories 的字典键仍按 session_key(见调用处),两者刻意分离。
            scope = getattr(event, "memory_scope", "") or session_key
            user_text = (getattr(event, "text", "") or "").strip()
            if user_text:
                wm.add(MemoryEntry(
                    type=MemoryType.USER, tier=MemoryTier.WORKING,
                    key="user", content=user_text[:500], source_session=scope,
                ))
            reply = (response_text or "").strip()
            if reply:
                wm.add(MemoryEntry(
                    type=MemoryType.USER, tier=MemoryTier.WORKING,
                    key="assistant", content=reply[:500], source_session=scope,
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

    async def _background_memory_review(self, messages: list[dict[str, Any]], session_key: str, memory_scope: str = "") -> None:
        from echo_agent.memory.reviewer import MemoryReviewer
        # R1 Task8:优先用 loop 注入的单例 service;缺省(旧构造/测试)才就近兜底。
        service = self._memory_service
        if service is None:
            from echo_agent.memory.service import MemoryService
            service = MemoryService(
                self._memory,
                invalidate_fn=self._invalidate_memory_caches_fn,
                flush_fn=getattr(self._memory, "flush_pending_embeds", None),
                allow_env_writes=self._config.memory.allow_model_environment_writes,
            )
        reviewer = MemoryReviewer(
            provider=self._provider,
            service=service,
            model=self._default_model,
            session_key=memory_scope or session_key,
        )
        # NB: exceptions propagate on purpose. This runs as a DURABLE factory, so
        # a raise lets the scheduler retry; swallowing it here would both hide
        # the failure AND (below) skip the counter reset — the counters would
        # only be cleared on a genuine success, so a failed review keeps the
        # nudge counters intact and next turn re-triggers this batch.
        #
        # The in-flight flag (dispatch dedupe, see finalize) is cleared in a
        # finally so it lifts on BOTH success and failure: on success the next
        # turn no longer re-triggers (counters are zeroed just below); on
        # failure the counters stay elevated, so lifting the flag lets the next
        # turn re-dispatch and retry the batch instead of wedging it forever.
        # The flag is only ever SET at the dispatch point (see finalize, under
        # the turn's session lock); this coroutine never re-sets it. A DURABLE
        # retry re-runs the whole coroutine, but that only re-enters this
        # finally and discards again — it never re-raises the flag. So the flag
        # is unconditionally cleared once (on success or failure), and a failed
        # review — whose nudge counters stay elevated — is re-dispatched (and
        # thus re-flagged) by a later turn rather than left stuck forever.
        try:
            actions = await reviewer.review(messages)
            if actions:
                logger.info("Background memory review: {}", "; ".join(actions))
                await self._clear_memory_snapshot(session_key)
            # Success: clear the memory-review nudge counters and persist. Done
            # here (not at the inference-stage trigger) so a failed/retried
            # review never loses the pending batch. Re-acquire under the session
            # lock so this does not race the next turn's own metadata write.
            await self._reset_memory_nudge_counters(session_key)
        finally:
            self._memory_review_inflight.discard(session_key)

    async def _reset_memory_nudge_counters(self, session_key: str) -> None:
        """Zero the memory-review nudge counters for ``session_key`` and persist.

        Serialized against the turn loop via the per-session lock so a
        concurrent next-turn save cannot clobber (or be clobbered by) this
        reset. No-op if the SessionManager stand-in lacks the lock/lookup API
        (older/lighter test doubles).

        The clear+save is wrapped in its own guard so a persistence failure
        does NOT propagate: this coroutine is awaited at the tail of the
        DURABLE ``_background_memory_review`` factory, so a raised save error
        would be caught by ``_run_durable`` and re-run the ENTIRE review
        (expensive LLM call + possibly duplicate memory writes). On save
        failure we ROLL BACK the in-memory counters to their prior value:
        Session is a shared cached object, so leaving it zeroed after a failed
        save would make the next turn read 0 and never re-trigger the review —
        the counters would be lost in memory yet never persisted."""
        acquire = getattr(self._sessions, "acquire", None)
        get_or_create = getattr(self._sessions, "get_or_create", None)
        if acquire is None or get_or_create is None:
            return
        try:
            lock = await acquire(session_key)
            async with lock:
                session = await get_or_create(session_key)
                old = (
                    session.metadata.get("_nudge_turns_memory", 0),
                    session.metadata.get("_nudge_tool_iters_memory", 0),
                )
                session.metadata["_nudge_turns_memory"] = 0
                session.metadata["_nudge_tool_iters_memory"] = 0
                try:
                    await self._sessions.save(session)
                except Exception as e:
                    # 回滚内存值,保持内存/磁盘一致,下一 turn 仍会重新触发 review。
                    session.metadata["_nudge_turns_memory"] = old[0]
                    session.metadata["_nudge_tool_iters_memory"] = old[1]
                    logger.warning(
                        "Memory nudge counter save failed for {}; counters restored "
                        "in-memory, next turn will re-trigger: {}",
                        session_key, e,
                    )
        except Exception as e:  # noqa: BLE001 — acquire/get_or_create 失败不上抛,避免 review 重跑
            logger.warning(
                "Memory nudge counter reset failed for {}: {}", session_key, e,
            )
