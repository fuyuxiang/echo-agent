"""Tests for the pipeline stages (context, inference, response)."""

import asyncio
from collections import OrderedDict
from dataclasses import dataclass
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.pipeline.types import PipelineContext, InferenceResult
from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.response_stage import ResponseStage, ProcessResult
from echo_agent.agent.tools.circuit_breaker import ToolCircuitBreaker
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
            tool_definitions_fn=lambda: [],
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

    def test_infer_task_type_code(self):
        stage = ContextStage(
            config=MagicMock(), sessions=MagicMock(), memory=MagicMock(),
            compressor=MagicMock(), context_builder=MagicMock(), skill_store=None,
            knowledge=None, hybrid_retriever=None, planner=None,
            inference=MagicMock(), working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(), snapshot_enabled=False,
            tool_definitions_fn=lambda: [],
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
            tool_definitions_fn=lambda: [],
        )
        assert stage._infer_task_type("搜索最新的新闻") == "research"

    def test_infer_task_type_chat(self):
        stage = ContextStage(
            config=MagicMock(), sessions=MagicMock(), memory=MagicMock(),
            compressor=MagicMock(), context_builder=MagicMock(), skill_store=None,
            knowledge=None, hybrid_retriever=None, planner=None,
            inference=MagicMock(), working_memories=OrderedDict(),
            memory_snapshots=OrderedDict(), snapshot_enabled=False,
            tool_definitions_fn=lambda: [],
        )
        assert stage._infer_task_type("你好") == "chat"


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
    async def test_finalize_strips_thinking(self):
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
