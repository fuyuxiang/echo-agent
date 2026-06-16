"""Context stage — assembles system prompt, memory, retrieval, and messages for LLM."""

from __future__ import annotations

import copy
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
    from echo_agent.agent.compression import ConversationCompressor
    from echo_agent.agent.planning.planner import AgentPlanner
    from echo_agent.config.schema import Config
    from echo_agent.knowledge.index import KnowledgeIndex
    from echo_agent.memory.retriever import HybridRetriever
    from echo_agent.memory.store import MemoryStore
    from echo_agent.models.inference import InferenceController
    from echo_agent.session.manager import SessionManager
    from echo_agent.skills.store import SkillStore


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
        snapshot_enabled: bool,
        tool_definitions_fn: Any,
        episodic: Any = None,
        plan_run_store: Any = None,
        bus: Any = None,
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
        self._snapshot_enabled = snapshot_enabled
        self._tool_definitions_fn = tool_definitions_fn
        self._episodic = episodic
        self._plan_run_store = plan_run_store
        self._bus = bus

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

        if self._snapshot_enabled:
            if event.session_key not in self._memory_snapshots:
                self._memory_snapshots[event.session_key] = self._memory.get_snapshot(
                    session_key=event.session_key
                )
                self._memory_snapshots.move_to_end(event.session_key)
            memory_ctx = build_memory_context(
                self._memory,
                snapshot=self._memory_snapshots.get(event.session_key, ""),
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
        if media_refs:
            session.add_message("user", event.text, media_refs=media_refs)
        else:
            session.add_message("user", event.text)

        retrieval_parts: list[str] = []
        if self._config.memory.enabled:
            if self._hybrid_retriever:
                scored = await self._hybrid_retriever.retrieve(
                    event.text, limit=5, session_key=event.session_key
                )
            else:
                scored = self._memory.search_scored(
                    event.text, limit=5, session_key=event.session_key
                )
            if scored:
                retrieval_parts.append(
                    "Relevant memory:\n"
                    + "\n".join(f"- {r.key}: {r.content}" for r, _ in scored)
                )
                if publish_response and self._bus:
                    _debug = getattr(self._config.gateway, 'progress_debug', False)
                    _mem_meta: dict[str, Any] = {
                        "progress_type": "memory_retrieved",
                        "count": len(scored),
                    }
                    if _debug:
                        _mem_meta["entries"] = [
                            {"key": r.key, "content_preview": r.content[:100]}
                            for r, _ in scored[:5]
                        ]
                    await self._emit_progress(event, _mem_meta)

            # Episodic recall — past conversation episodes for this session.
            # Episodes are written by the consolidator but were previously never
            # read back; surfacing the most relevant ones gives cross-session
            # continuity beyond the distilled semantic facts above.
            if self._episodic is not None:
                try:
                    episodes = await self._episodic.search_episodes(
                        event.text, session_key=event.session_key, limit=3
                    )
                    if not episodes:
                        episodes = await self._episodic.get_session_episodes(
                            event.session_key, limit=3
                        )
                    if episodes:
                        retrieval_parts.append(
                            "Past episodes:\n"
                            + "\n".join(f"- {ep.summary}" for ep in episodes if ep.summary)
                        )
                except Exception as e:
                    logger.debug("Episodic recall failed: {}", e)

        if self._knowledge:
            knowledge_results = self._knowledge.search(
                event.text,
                limit=self._config.knowledge.max_results,
                user_id=event.sender_id,
            )
            knowledge_context = self._knowledge.format_results(knowledge_results)
            if knowledge_context:
                retrieval_parts.append(knowledge_context)
                if publish_response and self._bus:
                    _debug = getattr(self._config.gateway, 'progress_debug', False)
                    _know_meta: dict[str, Any] = {
                        "progress_type": "knowledge_cited",
                        "count": len(knowledge_results) if knowledge_results else 0,
                    }
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
            current_message=event.text,
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
