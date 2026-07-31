"""Tests for the pipeline stages (context, inference, response)."""

import asyncio
from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.types import PipelineContext, InferenceResult
from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.response_stage import ResponseStage
from echo_agent.bus.events import InboundEvent
from echo_agent.session.manager import Session


class TestPipelineTypes:
    def test_pipeline_context_defaults(self):
        event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="test:c1")
        ctx = PipelineContext(event=event, session=session, trace_id="abc", publish_response=False)
        assert ctx.task_type == "chat"
        assert ctx.messages == []

    def test_inference_result_defaults(self):
        result = InferenceResult()
        assert result.response_text == ""
        assert result.total_tool_calls == 0


class TestContextStage:
    @pytest.mark.asyncio
    async def test_builds_context_with_minimal_config(self):
        config = MagicMock()
        config.session.max_history_messages = 100
        config.memory.enabled = False
        config.knowledge = MagicMock()
        config.knowledge.enabled = False

        sessions = AsyncMock()
        sessions.save = AsyncMock()

        memory = MagicMock()
        memory.get_snapshot = MagicMock(return_value="")
        memory.search_scored = MagicMock(return_value=[])

        compressor = MagicMock()
        compressor.should_compress = MagicMock(return_value=False)

        context_builder = MagicMock()
        context_builder.build_system_prompt = MagicMock(return_value="You are a helpful assistant.")
        context_builder.build_messages = MagicMock(return_value=[
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "hello"},
        ])

        inference = MagicMock()
        inference.filter_tools = MagicMock(return_value=[])

        stage = ContextStage(
            config=config,
            sessions=sessions,
            memory=memory,
            compressor=compressor,
            context_builder=context_builder,
            skill_store=None,
            knowledge=None,
            hybrid_retriever=None,
            planner=None,
            inference=inference,
            working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(),
            snapshot_enabled=False,
            tool_definitions_fn=lambda channel=None: [],
        )

        event = InboundEvent.text_message(channel="cli", sender_id="user", chat_id="c1", text="hello")
        session = Session(key="cli:c1")

        ctx = await stage.build(
            event, session,
            publish_response=False,
            trace_id="t1",
            stream_publisher=None,
            intro_text="",
        )

        assert ctx.event == event
        assert ctx.session == session
        assert ctx.task_type == "chat"
        assert len(ctx.messages) == 2
        assert ctx.tool_defs == []

    @pytest.mark.asyncio
    async def test_passes_this_turns_channel_to_tool_defs_and_prompt(self):
        """The channel must reach both schema generation and the system prompt.

        This is the one line that wires per-channel tool schemas into production
        (clarify must not promise an IM user a picker). Without this test, dropping
        the argument leaves every other pipeline test green, because their stubs
        accept the channel and discard it.
        """
        config = MagicMock()
        config.session.max_history_messages = 100
        config.memory.enabled = False
        config.knowledge = MagicMock()
        config.knowledge.enabled = False

        memory = MagicMock()
        memory.get_snapshot = MagicMock(return_value="")
        memory.search_scored = MagicMock(return_value=[])

        compressor = MagicMock()
        compressor.should_compress = MagicMock(return_value=False)

        context_builder = MagicMock()
        context_builder.build_system_prompt = MagicMock(return_value="sys")
        context_builder.build_messages = MagicMock(return_value=[])

        inference = MagicMock()
        inference.filter_tools = MagicMock(side_effect=lambda defs: defs)

        seen: list[str | None] = []

        def tool_definitions_fn(channel=None):
            seen.append(channel)
            return []

        stage = ContextStage(
            config=config,
            sessions=AsyncMock(),
            memory=memory,
            compressor=compressor,
            context_builder=context_builder,
            skill_store=None,
            knowledge=None,
            hybrid_retriever=None,
            planner=None,
            inference=inference,
            working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(),
            snapshot_enabled=False,
            tool_definitions_fn=tool_definitions_fn,
        )

        event = InboundEvent.text_message(
            channel="weixin:group", sender_id="user", chat_id="c1", text="hello",
        )
        await stage.build(
            event, Session(key="weixin:group:c1"),
            publish_response=False, trace_id="t1", stream_publisher=None, intro_text="",
        )

        assert seen == ["weixin:group"]
        assert context_builder.build_system_prompt.call_args.kwargs["channel"] == "weixin:group"

    def test_infer_task_type_code(self):
        stage = ContextStage(
            config=MagicMock(), sessions=MagicMock(), memory=MagicMock(),
            compressor=MagicMock(), context_builder=MagicMock(), skill_store=None,
            knowledge=None, hybrid_retriever=None, planner=None,
            inference=MagicMock(), working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(), snapshot_enabled=False,
            tool_definitions_fn=lambda channel=None: [],
        )
        assert stage._infer_task_type("帮我写一个python函数") == "code"
        assert stage._infer_task_type("fix this bug please") == "code"

    def test_infer_task_type_research(self):
        stage = ContextStage(
            config=MagicMock(), sessions=MagicMock(), memory=MagicMock(),
            compressor=MagicMock(), context_builder=MagicMock(), skill_store=None,
            knowledge=None, hybrid_retriever=None, planner=None,
            inference=MagicMock(), working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(), snapshot_enabled=False,
            tool_definitions_fn=lambda channel=None: [],
        )
        assert stage._infer_task_type("搜索最新的新闻") == "research"

    def test_infer_task_type_chat(self):
        stage = ContextStage(
            config=MagicMock(), sessions=MagicMock(), memory=MagicMock(),
            compressor=MagicMock(), context_builder=MagicMock(), skill_store=None,
            knowledge=None, hybrid_retriever=None, planner=None,
            inference=MagicMock(), working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(), snapshot_enabled=False,
            tool_definitions_fn=lambda channel=None: [],
        )
        assert stage._infer_task_type("你好") == "chat"

    @pytest.mark.asyncio
    async def test_reply_quote_enters_current_turn_prompt(self):
        """带引用回复时,被引用原文必须进入本轮 prompt(current_message),
        而不只是写进历史等下一轮才出现。检索/压缩仍用原始 event.text。"""
        config = MagicMock()
        config.session.max_history_messages = 100
        config.session.history_image_ttl_minutes = 30
        config.session.history_image_limit = 4
        config.session.history_image_skip_if_current = True
        config.memory.enabled = False
        config.knowledge = MagicMock()
        config.knowledge.enabled = False

        sessions = AsyncMock()
        sessions.save = AsyncMock()

        memory = MagicMock()
        memory.get_snapshot = MagicMock(return_value="")
        compressor = MagicMock()
        compressor.should_compress = MagicMock(return_value=False)

        context_builder = MagicMock()
        context_builder.build_system_prompt = MagicMock(return_value="sys")
        context_builder.build_messages = MagicMock(return_value=[
            {"role": "user", "content": "x"},
        ])

        inference = MagicMock()
        inference.filter_tools = MagicMock(return_value=[])

        stage = ContextStage(
            config=config, sessions=sessions, memory=memory,
            compressor=compressor, context_builder=context_builder,
            skill_store=None, knowledge=None, hybrid_retriever=None,
            planner=None, inference=inference, working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(), snapshot_enabled=False,
            tool_definitions_fn=lambda channel=None: [],
        )

        event = InboundEvent.text_message(
            channel="cli", sender_id="user", chat_id="c1", text="改改这个",
            reply_to_text="原始方案", reply_to_sender="Alice",
        )
        session = Session(key="cli:c1")

        await stage.build(
            event, session, publish_response=False, trace_id="t1",
            stream_publisher=None, intro_text="",
        )

        _, kwargs = context_builder.build_messages.call_args
        current = kwargs["current_message"]
        assert "原始方案" in current and "Alice" in current
        # 原始 event.text 不被污染(上游检索/压缩仍用原问题)
        assert event.text == "改改这个"


class TestResponseStage:
    @pytest.mark.asyncio
    async def test_finalize_saves_session(self):
        sessions = AsyncMock()
        sessions.save = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=False)

        spawned = []

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="test-model",
            spawn_fn=lambda coro: spawned.append(coro),
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="cli", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="cli:c1")
        ctx = PipelineContext(
            event=event, session=session, trace_id="t1", publish_response=False,
        )
        result = InferenceResult(response_text="Hello!")

        process_result = await stage.finalize(ctx, result)

        assert process_result.response_text == "Hello!"
        sessions.save.assert_called_once()
        assert session.messages[-1]["content"] == "Hello!"

    @pytest.mark.asyncio
    async def test_pure_chat_triggers_memory_review(self):
        """Regression: memory review must fire for pure chat (no tool calls).

        Previously gated on `total_tool_calls > 0`, so personal facts shared in
        plain conversation were never reviewed/persisted.
        """
        sessions = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=False)

        spawned = []

        def _record_spawn(coro, **kwargs):
            # Memory review is now dispatched as a DURABLE zero-arg factory
            # (retry-capable), so record the item + tier without awaiting it.
            spawned.append((coro, kwargs.get("tier")))

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=_record_spawn,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="weixin", sender_id="u1", chat_id="c1", text="我生日是10月11日")
        session = Session(key="weixin:c1")
        ctx = PipelineContext(event=event, session=session, trace_id="t1", publish_response=False)
        # Pure chat: no tool calls, but review was requested by the turn-based trigger.
        result = InferenceResult(response_text="好的", total_tool_calls=0, should_review_memory=True)

        await stage.finalize(ctx, result)

        # The memory review must have been spawned despite zero tool calls, and
        # it must be DURABLE (a dropped/failed review would otherwise lose the
        # batch — E4).
        from echo_agent.agent.background import Tier
        assert len(spawned) == 1
        assert spawned[0][1] == Tier.DURABLE

    @pytest.mark.asyncio
    async def test_skill_review_still_requires_tool_calls(self):
        """Skill review reviews tool-usage patterns, so it stays gated on tool calls."""
        sessions = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=False)

        spawned = []

        def _record_spawn(coro):
            spawned.append(coro)
            coro.close()

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=_record_spawn,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="cli", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="cli:c1")
        ctx = PipelineContext(event=event, session=session, trace_id="t1", publish_response=False)
        result = InferenceResult(response_text="hi", total_tool_calls=0, should_review_skills=True)

        await stage.finalize(ctx, result)

        # Skill review should NOT spawn with zero tool calls.
        assert len(spawned) == 0

        sessions = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=False)

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=lambda coro: None,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="cli", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="cli:c1")
        ctx = PipelineContext(event=event, session=session, trace_id="t1", publish_response=False)
        result = InferenceResult(response_text="<think>internal</think>Visible response")

        process_result = await stage.finalize(ctx, result)
        assert "internal" not in process_result.response_text
        assert "Visible response" in process_result.response_text

    @pytest.mark.asyncio
    async def test_eval_channel_skips_consolidation_and_review(self):
        """Eval/test traffic must not feed long-term memory or memory review."""
        sessions = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=True)
        consolidation.schedule = AsyncMock()

        spawned = []

        def _record_spawn(coro):
            spawned.append(coro)
            coro.close()

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=_record_spawn,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="eval", sender_id="u1", chat_id="list_skills", text="x")
        session = Session(key="eval:list_skills")
        ctx = PipelineContext(event=event, session=session, trace_id="t1", publish_response=False)
        result = InferenceResult(response_text="ok", total_tool_calls=0, should_review_memory=True)

        await stage.finalize(ctx, result)

        consolidation.schedule.assert_not_called()
        assert len(spawned) == 0

    @pytest.mark.asyncio
    async def test_embedding_flush_is_durable_factory(self):
        """Task 8: flushing pending embeddings is a DURABLE point — it must be
        spawned with tier=DURABLE and as a zero-arg factory (retry-capable)."""
        from echo_agent.agent.background import Tier

        sessions = AsyncMock()
        sessions.save = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=True)
        memory.flush_pending_embeds = AsyncMock(return_value=0)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=False)

        seen = []

        def _record_spawn(coro, *, session_key="", tier=None):
            seen.append((coro, tier))
            if asyncio.iscoroutine(coro):
                coro.close()

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=_record_spawn,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="cli", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="cli:c1")
        ctx = PipelineContext(event=event, session=session, trace_id="t1", publish_response=False)
        result = InferenceResult(response_text="ok")

        await stage.finalize(ctx, result)

        flush_spawns = [s for s in seen if s[1] is Tier.DURABLE]
        assert len(flush_spawns) == 1
        coro, _ = flush_spawns[0]
        assert callable(coro) and not asyncio.iscoroutine(coro)

    @pytest.mark.asyncio
    async def test_consolidation_uses_durable_tier(self):
        """Task 8: consolidation.schedule must be invoked with tier=DURABLE."""
        from echo_agent.agent.background import Tier

        sessions = AsyncMock()
        sessions.save = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=True)
        consolidation.schedule = AsyncMock()

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=lambda coro, **kw: None,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="cli", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="cli:c1")
        session.add_message("user", "hi")
        ctx = PipelineContext(event=event, session=session, trace_id="t1", publish_response=False)
        result = InferenceResult(response_text="ok")

        await stage.finalize(ctx, result)

        consolidation.schedule.assert_called_once()
        assert consolidation.schedule.call_args.kwargs.get("tier") is Tier.DURABLE

    @pytest.mark.asyncio
    async def test_finalize_prepends_intro(self):
        sessions = AsyncMock()
        memory = MagicMock()
        memory.has_pending_embeds = MagicMock(return_value=False)

        consolidation = MagicMock()
        consolidation._consolidator = MagicMock()
        consolidation._consolidator.should_consolidate = MagicMock(return_value=False)

        stage = ResponseStage(
            config=MagicMock(),
            sessions=sessions,
            memory=memory,
            provider=MagicMock(),
            consolidation_worker=consolidation,
            default_model="m",
            spawn_fn=lambda coro: None,
            clear_memory_snapshot_fn=AsyncMock(),
        )

        event = InboundEvent.text_message(channel="cli", sender_id="u1", chat_id="c1", text="hi")
        session = Session(key="cli:c1")
        ctx = PipelineContext(
            event=event, session=session, trace_id="t1",
            publish_response=False, intro_text="Welcome!",
        )
        result = InferenceResult(response_text="How can I help?")

        process_result = await stage.finalize(ctx, result)
        assert process_result.response_text.startswith("Welcome!")
        assert "How can I help?" in process_result.response_text
