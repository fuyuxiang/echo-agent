"""Context stage — assembles system prompt, memory, retrieval, and messages for LLM."""

from __future__ import annotations

import copy
import time
from collections import OrderedDict
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.context import (
    ContextBuilder,
    build_capabilities_context,
    build_memory_context,
    build_skills_context,
)
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.session.manager import Session

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from echo_agent.agent.compression import ConversationCompressor
    from echo_agent.agent.cognitive_emitter import CognitiveEmitter
    from echo_agent.agent.planning.planner import AgentPlanner
    from echo_agent.config.schema import Config
    from echo_agent.knowledge.index import KnowledgeIndex
    from echo_agent.memory.retriever import HybridRetriever
    from echo_agent.memory.store import MemoryStore
    from echo_agent.models.inference import InferenceController
    from echo_agent.session.manager import SessionManager
    from echo_agent.skills.store import SkillStore


def filter_recall_by_snapshot(scored, snapshot_ids):
    """Drop scored memory entries whose id already entered the frozen snapshot.
    Defensive: entries without an id, or a falsy snapshot_ids, are kept as-is."""
    if not snapshot_ids:
        return scored
    return [
        (r, s) for (r, s) in scored
        if getattr(r, "id", None) not in snapshot_ids
    ]


_REPLY_SNIPPET_MAX = 500  # 被引用原文注入上限，过长截断，避免撑爆上下文


def build_user_message_with_reply(event: InboundEvent) -> str:
    """构造写入会话历史的用户消息文本，把被引用消息原文作为前缀注入。

    跨通道统一的「理解层」：让模型知道用户在针对历史里哪一条消息发问（消歧），
    而不是只看到用户这次的新文字。无被引用原文时原样返回 event.text，不注入。
    注意只影响写入历史的副本，不改 event.text（检索/压缩仍用原始问题）。
    """
    reply_text = (event.reply_to_text or "").strip()
    if not reply_text:
        return event.text
    snippet = reply_text[:_REPLY_SNIPPET_MAX]
    if event.reply_to_is_own:
        prefix = f'[回复你刚才的消息: "{snippet}"]'
    elif event.reply_to_sender:
        prefix = f'[引用 {event.reply_to_sender}: "{snippet}"]'
    else:
        prefix = f'[引用: "{snippet}"]'
    return f"{prefix}\n\n{event.text}" if event.text else prefix


class ContextStage:
    """Builds the full pipeline context: system prompt, messages, retrieval, tool defs."""

    _TASK_MARKERS = {
        "code": ("代码", "报错", "bug", "函数", "class ", "def ", "typescript", "python"),
        "research": ("搜索", "查找", "search", "find", "look up", "查一下"),
        "planning": ("计划", "规划", "plan", "schedule", "安排"),
    }

    def __init__(
        self,
        *,
        config: Config,
        sessions: SessionManager,
        memory: MemoryStore,
        compressor: ConversationCompressor,
        context_builder: ContextBuilder,
        skill_store: SkillStore | None,
        knowledge: KnowledgeIndex | None,
        hybrid_retriever: HybridRetriever | None,
        planner: AgentPlanner | None,
        inference: InferenceController,
        working_memories: OrderedDict,
        memory_snapshots: OrderedDict,
        memory_snapshot_ids: "OrderedDict | None" = None,
        put_snapshot: "Callable[[str, str, frozenset], Awaitable[None]] | None" = None,
        snapshot_enabled: bool,
        tool_definitions_fn: Any,
        episodic: Any = None,
        plan_run_store: Any = None,
        bus: Any = None,
        retrieval_cache_get: "Callable[[str], Any] | None" = None,
        retrieval_on_miss: str = "degrade",
        cache_ttl: float = 60.0,
        cache_jaccard_min: float = 0.3,
        cognitive_emitter: "CognitiveEmitter | None" = None,
    ):
        self._config = config
        self._sessions = sessions
        self._memory = memory
        self._compressor = compressor
        self._context_builder = context_builder
        self._skill_store = skill_store
        self._knowledge = knowledge
        self._hybrid_retriever = hybrid_retriever
        self._planner = planner
        self._inference = inference
        self._working_memories = working_memories
        self._memory_snapshots = memory_snapshots
        self._memory_snapshot_ids = memory_snapshot_ids if memory_snapshot_ids is not None else {}
        self._put_snapshot = put_snapshot
        self._snapshot_enabled = snapshot_enabled
        self._tool_definitions_fn = tool_definitions_fn
        self._episodic = episodic
        self._plan_run_store = plan_run_store
        self._bus = bus
        self._retrieval_cache_get = retrieval_cache_get
        self._retrieval_on_miss = retrieval_on_miss
        self._cache_ttl = cache_ttl
        self._cache_jaccard_min = cache_jaccard_min
        self._cog = cognitive_emitter

    async def _emit_progress(self, event: InboundEvent, metadata: dict[str, Any]) -> None:
        if not getattr(self._config.gateway, 'emit_progress_events', True):
            return
        out = OutboundEvent.text_reply(
            channel=event.channel, chat_id=event.chat_id, text="", reply_to_id=event.reply_to_id,
        )
        out.is_final = False
        out.message_kind = "progress"
        out.metadata = {"_progress": True, "_inbound_event_id": event.event_id}
        out.metadata.update(metadata)
        await self._bus.publish_outbound(out)

    async def _emit_memory_recalled(self, event: InboundEvent, scored: list) -> None:
        """Emit a `memory_recalled` cognitive frame carrying each recalled
        memory's content/source-grade/score. Called from build() after the
        turn's memory items are known.

        Normalizes BOTH shapes deliberately: the real call site passes
        `(entry, score)` tuples (entry has `.content`/`.source`), while unit
        tests and any structured-dict caller pass `{"content","source","score"}`
        dicts. Handling both keeps the emitter robust to either provenance
        without a shared type dependency — not overbuilding.
        """
        # Gate before building items: on IM channels this skips the whole
        # slice/round loop, not just a discarded emit() payload.
        if self._cog is None or not scored or not self._cog.active(event):
            return
        items: list[dict[str, Any]] = []
        for s in scored[:12]:
            if isinstance(s, dict):
                content = s.get("content", "")
                source = s.get("source", "legacy")
                score = s.get("score", 0.0)
            elif isinstance(s, tuple):  # real call site: (entry, score)
                entry, score = s[0], (s[1] if len(s) > 1 else 0.0)
                content = getattr(entry, "content", "")
                source = getattr(entry, "source", "legacy")
            else:  # bare entry object
                content = getattr(s, "content", str(s))
                source = getattr(s, "source", "legacy")
                score = getattr(s, "score", 0.0)
            items.append({
                "content": str(content)[:200],
                "source": source or "legacy",
                "score": round(float(score or 0.0), 3),
            })
        await self._cog.emit(
            event, "memory_recalled", {"items": items},
            f"召回 {len(items)} 条记忆",
        )

    async def _fetch_knowledge(self, query: str, user_id: str) -> tuple[list, str]:
        """Inline knowledge retrieval. Vector path is async; keyword-only path
        degrades internally. Scoped by user_id for access control."""
        results = await self._knowledge.search_async(
            query, limit=self._config.knowledge.max_results, user_id=user_id
        )
        context = self._knowledge.format_results(results)
        return results, context

    async def build(
        self,
        event: InboundEvent,
        session: Session,
        *,
        publish_response: bool,
        trace_id: str,
        stream_publisher: Any,
        intro_text: str,
    ) -> PipelineContext:
        working_ctx = ""
        if event.session_key in self._working_memories:
            working_ctx = self._working_memories[event.session_key].get_context()

        snapshot_ids: frozenset[str] = frozenset()
        if self._snapshot_enabled:
            if event.session_key in self._memory_snapshots:
                snapshot = self._memory_snapshots[event.session_key]
                snapshot_ids = self._memory_snapshot_ids.get(event.session_key, frozenset())
            else:
                snapshot, snapshot_ids = self._memory.get_snapshot_with_ids(
                    session_key=event.session_key
                )
                if self._put_snapshot is not None:
                    # 写入唯一入口经 loop 的统一 LRU(锁 + 上限);
                    # put_snapshot 为空时本轮仍用刚算出的 snapshot,但不缓存
                    # (不得回退到无界直写 dict)。
                    await self._put_snapshot(event.session_key, snapshot, snapshot_ids)
            memory_ctx = build_memory_context(
                self._memory,
                snapshot=snapshot,
                working_memory=working_ctx,
            )
        else:
            memory_ctx = build_memory_context(
                self._memory, session_key=event.session_key, working_memory=working_ctx
            )

        skills_ctx = build_skills_context(self._skill_store)
        # Derive capabilities from the live tool registry (config, not memory).
        tool_defs = self._inference.filter_tools(self._tool_definitions_fn())
        capabilities_ctx = build_capabilities_context(tool_defs)
        system_prompt = self._context_builder.build_system_prompt(
            memory_context=memory_ctx,
            skills_context=skills_ctx,
            capabilities=capabilities_ctx,
        )

        history = session.get_history(self._config.session.max_history_messages)
        if self._compressor.should_compress(history):
            self._compressor._session_key = event.session_key
            history_copy = copy.deepcopy(history)
            result = await self._compressor.compress(history_copy, focus_topic=event.text)
            history = result.messages
            if result.was_compressed:
                logger.info(
                    "Context compressed: {} → {} tokens",
                    result.tokens_before,
                    result.tokens_after,
                )
                session.messages = session.messages[:session.last_consolidated] + result.messages
                await self._sessions.save(session)

        media_items = event.media_items
        resolved_media = (
            await self._context_builder.resolve_inbound_media(media_items, event.channel)
            if media_items
            else None
        )

        media_refs = self._build_media_refs(resolved_media) if resolved_media else None
        # 引用回复：把被引用消息原文作为前缀注入写入历史的文本，供模型消歧。
        # 只影响历史副本，event.text 保持原样（上游检索/压缩仍用原始问题）。
        user_message = build_user_message_with_reply(event)
        if media_refs:
            session.add_message("user", user_message, media_refs=media_refs)
        else:
            session.add_message("user", user_message)

        retrieval_parts: list[str] = []
        from echo_agent.memory.prefetch import is_fresh

        # A single per-session prefetched entry warms main memory + episodic +
        # knowledge together (Task 13). Fetch it once here so all three segments
        # below share one freshness decision. Knowledge lives outside the
        # memory.enabled block, so the lookup must sit above it.
        cached = (
            self._retrieval_cache_get(event.session_key)
            if self._retrieval_cache_get is not None
            else None
        )
        cache_fresh = cached is not None and is_fresh(
            cached, event.text, now=time.time(),
            ttl=self._cache_ttl, jaccard_min=self._cache_jaccard_min,
        )
        if self._config.memory.enabled:
            # Prefer a fresh prefetched result (zero inline latency). On a miss,
            # `retrieval_on_miss` decides: "sync" pays the retrieval latency this
            # turn (accuracy-first daemon/gateway), "degrade" skips retrieval this
            # turn (latency-first CLI) and emits a progress notice.
            scored = None
            if cache_fresh:
                scored = cached.scored
            elif self._retrieval_on_miss == "sync":
                if self._hybrid_retriever:
                    # Episode candidates are assembled inside retrieve() by
                    # relevance (semantic + LIKE), same as the prefetch path —
                    # no separate "recent N" fetch here, which would otherwise
                    # miss high-relevance episodes outside the recency window.
                    scored = await self._hybrid_retriever.retrieve(
                        event.text, limit=8, session_key=event.session_key,
                    )
                else:
                    scored = self._memory.search_scored(
                        event.text, limit=5, session_key=event.session_key
                    )
            elif publish_response and self._bus:
                # CLI degrade: no retrieval this turn — tell the user we skipped.
                await self._emit_progress(
                    event, {"progress_type": "memory_retrieval_skipped"}
                )
            if scored:
                scored = filter_recall_by_snapshot(scored, snapshot_ids)
            if scored:
                from echo_agent.memory.types import Episode as _Ep
                mem_items = [(r, s) for r, s in scored if not isinstance(r, _Ep)]
                ep_items = [(r, s) for r, s in scored if isinstance(r, _Ep)]
                if mem_items:
                    retrieval_parts.append(
                        "Relevant memory:\n"
                        + "\n".join(f"- {r.key}: {r.content}" for r, _ in mem_items)
                    )
                    try:
                        self._memory.reinforce([r.id for r, _ in mem_items])
                    except Exception as e:
                        logger.debug("Memory reinforcement failed: {}", e)
                if ep_items:
                    retrieval_parts.append(
                        "Past episodes:\n"
                        + "\n".join(f"- {r.summary}" for r, _ in ep_items if r.summary)
                    )
                if (mem_items or ep_items) and publish_response and self._bus:
                    _debug = getattr(self._config.gateway, 'progress_debug', False)
                    _mem_meta: dict[str, Any] = {
                        "progress_type": "memory_retrieved",
                        "count": len(mem_items) + len(ep_items),
                    }
                    if _debug:
                        _mem_meta["entries"] = [
                            {"key": r.key, "content_preview": r.content[:100]}
                            for r, _ in mem_items[:5]
                        ]
                    await self._emit_progress(event, _mem_meta)
                # Cognitive埋点: surface the actual recalled memories (content/
                # source-grade/score) to the CLI TUI. Pass mem_items (memory
                # entries only), NOT `scored` which mixes in episodes. The
                # emitter internally gates to channel=="gateway:cli".
                if mem_items and self._cog is not None:
                    await self._emit_memory_recalled(event, mem_items)

        if self._knowledge:
            # Knowledge is ACL-filtered per user (KnowledgeIndex.search filters
            # by allowed_users). A cached knowledge_context is only trustworthy
            # when it was prefetched for THIS turn's user: under a shared group
            # session_key the cache entry is reachable by every sender, so
            # serving user A's ACL-filtered knowledge to user B would leak
            # restricted docs. So the cache hit requires knowledge_user_id to
            # match the current sender.
            knowledge_context = None
            knowledge_results = None
            knowledge_from_cache = False
            cached_user_ok = (
                cache_fresh
                and cached.knowledge_context is not None
                and bool(event.sender_id)
                and cached.knowledge_user_id == event.sender_id
            )
            if cached_user_ok:
                knowledge_context = cached.knowledge_context
                knowledge_from_cache = True
            else:
                # Miss (no/stale cache, or knowledge was prefetched for another
                # user). The scan is CPU-bound, so any inline fetch runs in an
                # executor thread and never blocks the event loop. When a
                # prefetcher is warming knowledge (hybrid retriever present),
                # honor retrieval_on_miss: "sync" fetches inline now, "degrade"
                # skips and lets the next prefetch warm it. When there is NO
                # prefetcher (memory disabled -> no hybrid retriever), nothing
                # will ever warm the cache, so a degrade-skip would silently
                # drop knowledge every turn — fall back to inline so knowledge
                # keeps working independently of memory.enabled (its pre-Task-13
                # behavior).
                #
                # Senderless entrypoints (sender_id == "") can never satisfy the
                # cache-hit guard above (it requires a non-empty sender to avoid
                # leaking one user's ACL-filtered knowledge to another under a
                # shared session_key). For them the prefetch is unusable, so a
                # degrade-skip would drop knowledge on every turn. Treat the
                # prefetch as inactive when there is no sender and fall back to
                # inline: the inline fetch passes the empty user_id, which the
                # index resolves to public (unrestricted) docs only — no leak.
                knowledge_prefetch_active = (
                    self._hybrid_retriever is not None and bool(event.sender_id)
                )
                if self._retrieval_on_miss == "sync" or not knowledge_prefetch_active:
                    try:
                        knowledge_results, knowledge_context = (
                            await self._fetch_knowledge(event.text, event.sender_id)
                        )
                    except Exception as e:
                        logger.debug("Knowledge retrieval failed: {}", e)
            if knowledge_context:
                retrieval_parts.append(knowledge_context)
                if publish_response and self._bus:
                    _debug = getattr(self._config.gateway, 'progress_debug', False)
                    _know_meta: dict[str, Any] = {
                        "progress_type": "knowledge_cited",
                    }
                    # On a cache hit we kept only the formatted context, not the
                    # result objects, so an honest event reports the hit rather
                    # than a misleading count=0 / empty citations.
                    if knowledge_from_cache:
                        _know_meta["from_cache"] = True
                    else:
                        _know_meta["count"] = (
                            len(knowledge_results) if knowledge_results else 0
                        )
                        if _debug:
                            _know_meta["citations"] = [
                                {"path": getattr(r, 'path', ''), "chunk_preview": getattr(r, 'text', '')[:200], "score": getattr(r, 'score', 0.0)}
                                for r in (knowledge_results[:5] if knowledge_results else [])
                            ]
                    await self._emit_progress(event, _know_meta)

        task_type = self._infer_task_type(event.text)

        retrieval = "\n\n".join(retrieval_parts)

        session_cfg = self._config.session
        messages = self._context_builder.build_messages(
            history=history,
            # 引用回复:本轮 prompt 也用带引用前缀的 user_message,让模型当轮就看到
            # 被引用原文(消歧);event.text 保持原样,上游检索/压缩仍用原始问题。
            current_message=user_message,
            media=resolved_media,
            channel=event.channel,
            chat_id=event.chat_id,
            system_prompt=system_prompt,
            retrieval_context=retrieval,
            history_image_ttl_minutes=session_cfg.history_image_ttl_minutes,
            history_image_limit=session_cfg.history_image_limit,
            history_image_skip_if_current=session_cfg.history_image_skip_if_current,
        )

        # tool_defs already computed above for capability derivation.

        execution_plan = None
        plan_run_id = ""
        if self._planner and tool_defs:
            try:
                token_est = len(event.text) // 4
                execution_plan = await self._planner.create_plan(
                    query=event.text,
                    tools=tool_defs,
                    context=retrieval,
                    token_estimate=token_est,
                )
                # A single-step plan ("reason and act iteratively") carries no
                # information — injecting it just burns prompt tokens.
                if execution_plan and len(execution_plan.steps) > 1:
                    plan_context = execution_plan.to_prompt()
                    last_content = messages[-1]["content"]
                    if isinstance(last_content, list):
                        last_content[0]["text"] += f"\n\n[Plan]\n{plan_context}"
                    else:
                        messages[-1]["content"] = (
                            last_content + f"\n\n[Plan]\n{plan_context}"
                        )
                    # Persist the multi-step plan so step progress is queryable
                    # and an interrupted long task can be resumed.
                    if self._plan_run_store is not None:
                        try:
                            plan_run_id = await self._plan_run_store.create(
                                event.session_key, trace_id, execution_plan
                            )
                        except Exception as e:
                            logger.debug("Plan run persistence failed: {}", e)
            except Exception as e:
                logger.debug("Planning failed, proceeding without plan: {}", e)

        return PipelineContext(
            event=event,
            session=session,
            trace_id=trace_id,
            publish_response=publish_response,
            system_prompt=system_prompt,
            messages=messages,
            tool_defs=tool_defs,
            retrieval=retrieval,
            task_type=task_type,
            execution_plan=execution_plan,
            plan_run_id=plan_run_id,
            intro_text=intro_text,
            stream_publisher=stream_publisher,
        )

    @staticmethod
    def _build_media_refs(resolved_media: list[dict[str, str]]) -> list[dict[str, Any]]:
        """Extract lightweight image references from resolved media for session storage."""
        import time

        from echo_agent.session.media_ref import MediaRef

        refs: list[dict[str, Any]] = []
        now = time.time()
        for item in resolved_media:
            if item.get("type") != "image":
                continue
            url = item.get("url", "")
            if not url:
                continue
            is_local = not url.startswith(("http://", "https://", "data:"))
            refs.append(MediaRef(
                cache_path=url if is_local else "",
                original_url=item.get("original_url", "") or (url if not is_local else ""),
                mime_type=item.get("mime_type", ""),
                timestamp=now,
                aes_key=item.get("aes_key", ""),
            ).to_dict())
        return refs

    def _infer_task_type(self, text: str) -> str:
        lower = text.lower()
        for task_type, markers in self._TASK_MARKERS.items():
            if any(marker in lower for marker in markers):
                return task_type
        return "chat"
