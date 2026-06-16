"""Tests for InferenceStage — LLM call loop with tool execution and circuit breaking."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.models.provider import LLMResponse, ToolCallRequest


def _make_config():
    config = MagicMock()
    config.skills = MagicMock()
    config.skills.creation_nudge_interval = 0
    config.memory = MagicMock()
    config.memory.memory_nudge_interval = 0
    return config


def _make_event():
    event = MagicMock()
    event.channel = "test"
    event.chat_id = "chat_1"
    event.reply_to_id = None
    event.event_id = "evt_001"
    event.metadata = {}
    event.session_key = "test:chat_1"
    event.sender_id = "user_1"
    event.text = "hello"
    return event


def _make_session():
    session = MagicMock()
    session.add_message = MagicMock()
    return session


def _make_ctx(event=None, session=None, messages=None, tool_defs=None, publish_response=True):
    ctx = PipelineContext(
        event=event or _make_event(),
        session=session or _make_session(),
        trace_id="trace_001",
        publish_response=publish_response,
    )
    ctx.messages = messages or [{"role": "user", "content": "hello"}]
    ctx.tool_defs = tool_defs or []
    ctx.stream_publisher = MagicMock()
    ctx.stream_publisher.on_delta = AsyncMock()
    ctx.execution_plan = None
    ctx.task_type = "chat"
    return ctx


def _make_stage(provider=None, tools=None, approval_gate=None, max_iterations=10, planner=None):
    config = _make_config()
    bus = AsyncMock()
    bus.publish_outbound = AsyncMock()
    prov = provider or AsyncMock()
    if not provider:
        prov.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="done", finish_reason="stop")
        )

    tools_reg = tools or MagicMock()
    if not tools:
        tools_reg.execute = AsyncMock()
        tools_reg.has = MagicMock(return_value=False)

    gate = approval_gate or AsyncMock()
    if not approval_gate:
        gate.check = AsyncMock(return_value=MagicMock(denial=None, approved_actions=frozenset()))

    tracer = MagicMock()
    tracer.start_span = MagicMock(return_value="span_1")
    tracer.end_span = MagicMock()

    telemetry = MagicMock()
    telemetry.available = False

    inference_ctrl = MagicMock()
    inference_ctrl.validate_response = MagicMock(return_value=[])

    circuit_breaker = MagicMock()
    circuit_breaker.get_unavailable_tools = MagicMock(return_value=set())
    circuit_breaker.record_success = MagicMock()
    circuit_breaker.record_failure = MagicMock()

    credentials = MagicMock()
    credentials.get_for_tool = MagicMock(return_value={})

    stage = InferenceStage(
        config=config,
        bus=bus,
        provider=prov,
        router=None,
        tools=tools_reg,
        approval_gate=gate,
        credentials=credentials,
        tracer=tracer,
        telemetry=telemetry,
        inference=inference_ctrl,
        circuit_breaker=circuit_breaker,
        default_model="test-model",
        max_iterations=max_iterations,
        planner=planner,
    )
    return stage, bus


class TestInferenceStageTextOnly:
    """Model returns plain text with no tool calls — immediate completion."""

    @pytest.mark.asyncio
    async def test_plain_text_response(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="Hello world", finish_reason="stop")
        )
        stage, bus = _make_stage(provider=provider)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert result.response_text == "Hello world"
        assert result.total_tool_calls == 0


class TestInferenceStageError:
    """Model returns finish_reason='error' — fallback text."""

    @pytest.mark.asyncio
    async def test_error_response_fallback(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="LLM error occurred", finish_reason="error")
        )
        stage, bus = _make_stage(provider=provider)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert "issue" in result.response_text.lower() or "try again" in result.response_text.lower()


class TestInferenceStageApprovalDenied:
    """Tool call rejected by ApprovalGate — returns rejection message."""

    @pytest.mark.asyncio
    async def test_tool_denied(self):
        from echo_agent.agent.tools.base import ToolResult

        provider = AsyncMock()
        tc = ToolCallRequest(id="call_1", name="exec", arguments={"command": "rm -rf /"})
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="Let me run that", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="Understood, denied.", finish_reason="stop"),
        ])

        denial = ToolResult(success=False, error="Permission denied: dangerous command")
        gate = AsyncMock()
        gate.check = AsyncMock(return_value=MagicMock(denial=denial, approved_actions=frozenset()))

        stage, bus = _make_stage(provider=provider, approval_gate=gate)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert result.total_tool_calls == 1
        # The second LLM call responds after seeing the denial
        assert result.response_text == "Understood, denied."


class TestInferenceStageRepeatGuard:
    """Same tool + arguments called >= 4 times is blocked."""

    @pytest.mark.asyncio
    async def test_repeat_call_blocked(self):
        tc = ToolCallRequest(id="call_1", name="search", arguments={"q": "test"})

        provider = AsyncMock()
        # Return tool calls 5 times, then stop
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="Done after block", finish_reason="stop"),
        ])

        tools_reg = MagicMock()
        tool_result = MagicMock(success=True, text="result", error=None, metadata={})
        tools_reg.execute = AsyncMock(return_value=tool_result)
        tools_reg.has = MagicMock(return_value=False)

        stage, bus = _make_stage(provider=provider, tools=tools_reg)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        # The 4th call should be blocked (threshold is 4)
        assert result.response_text == "Done after block"


class TestInferenceStageMaxIterations:
    """Loop exhausts max_iterations — fallback text."""

    @pytest.mark.asyncio
    async def test_max_iterations_fallback(self):
        tc = ToolCallRequest(id="call_1", name="tool_a", arguments={"x": "1"})

        provider = AsyncMock()
        # Always return tool calls, never stop
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls")
        )

        tools_reg = MagicMock()
        tool_result = MagicMock(success=True, text="ok", error=None, metadata={})
        tools_reg.execute = AsyncMock(return_value=tool_result)
        tools_reg.has = MagicMock(return_value=False)

        stage, bus = _make_stage(provider=provider, tools=tools_reg, max_iterations=3)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        # Loop exhausted, fallback text should mention issue/try again
        assert "issue" in result.response_text.lower() or "try again" in result.response_text.lower()


class TestInferenceStageReflection:
    """反思闭环：多步 plan 上触发 reflect，should_replan=True 时重跑一次。"""

    def _make_multistep_plan(self):
        from echo_agent.agent.planning.models import Plan, PlanStep, StrategyType
        return Plan(
            strategy=StrategyType.PLAN_EXECUTE,
            steps=[PlanStep(index=0, description="a"), PlanStep(index=1, description="b")],
            goal="multi",
        )

    def _make_feedback(self, *, should_replan: bool, critique: str = "", suggestions: list | None = None):
        from echo_agent.agent.planning.models import Feedback
        return Feedback(
            should_replan=should_replan,
            critique=critique,
            suggestions=suggestions or [],
        )

    @pytest.mark.asyncio
    async def test_reflection_triggers_rerun_when_should_replan(self):
        """planner.reflect 返回 should_replan=True 时应触发第二轮推理。"""
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="partial", finish_reason="stop"),
            LLMResponse(content="final", finish_reason="stop"),
        ])

        planner = AsyncMock()
        planner.reflect = AsyncMock(
            return_value=self._make_feedback(should_replan=True, critique="incomplete", suggestions=["do x"])
        )

        stage, _ = _make_stage(provider=provider, planner=planner)
        ctx = _make_ctx()
        ctx.execution_plan = self._make_multistep_plan()

        result = await stage.run(ctx)

        assert result.response_text == "final"
        planner.reflect.assert_called_once()
        assert provider.chat_stream_with_retry.call_count == 2
        # 验证反思引导消息已追加进 messages
        reflection_msgs = [m for m in ctx.messages if m.get("role") == "user" and "[Reflection]" in m.get("content", "")]
        assert len(reflection_msgs) == 1

    @pytest.mark.asyncio
    async def test_reflection_no_rerun_when_satisfied(self):
        """planner.reflect 返回 should_replan=False 时不应重跑。"""
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="first", finish_reason="stop")
        )

        planner = AsyncMock()
        planner.reflect = AsyncMock(
            return_value=self._make_feedback(should_replan=False)
        )

        stage, _ = _make_stage(provider=provider, planner=planner)
        ctx = _make_ctx()
        ctx.execution_plan = self._make_multistep_plan()

        result = await stage.run(ctx)

        assert result.response_text == "first"
        assert provider.chat_stream_with_retry.call_count == 1

    @pytest.mark.asyncio
    async def test_reflection_skipped_for_single_step_plan(self):
        """单步 plan 不应触发 reflect。"""
        from echo_agent.agent.planning.models import Plan, PlanStep, StrategyType

        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="done", finish_reason="stop")
        )

        planner = AsyncMock()
        planner.reflect = AsyncMock()

        stage, _ = _make_stage(provider=provider, planner=planner)
        ctx = _make_ctx()
        ctx.execution_plan = Plan(
            strategy=StrategyType.PLAN_EXECUTE,
            steps=[PlanStep(index=0, description="only step")],
            goal="single",
        )

        await stage.run(ctx)

        assert not planner.reflect.called

    @pytest.mark.asyncio
    async def test_reflection_rerun_accumulates_nudge_counters(self):
        """重跑时两轮的工具调用计数应完整累计写回 session，不丢第一轮。"""
        tc = ToolCallRequest(id="c", name="search", arguments={"q": "x"})
        provider = AsyncMock()
        # 第一轮：1 次工具调用后 stop；第二轮：1 次工具调用后 stop
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="r1", finish_reason="stop"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="r2", finish_reason="stop"),
        ])

        tools_reg = MagicMock()
        tools_reg.execute = AsyncMock(return_value=MagicMock(success=True, text="ok", error=None, metadata={}))
        tools_reg.has = MagicMock(return_value=False)

        planner = AsyncMock()
        planner.reflect = AsyncMock(
            return_value=self._make_feedback(should_replan=True, critique="more")
        )

        # 真 dict metadata，才能验证计数累计
        session = MagicMock()
        session.add_message = MagicMock()
        session.metadata = {}

        stage, _ = _make_stage(provider=provider, tools=tools_reg, planner=planner)
        ctx = _make_ctx(session=session)
        ctx.execution_plan = self._make_multistep_plan()

        await stage.run(ctx)

        # 整 turn 共 2 次工具调用，计数器应为 2（而非只数第二轮的 1）
        assert session.metadata["_nudge_tool_iters_skill"] == 2
        assert session.metadata["_nudge_tool_iters_memory"] == 2

    @pytest.mark.asyncio
    async def test_reflection_skipped_when_no_planner(self):
        """不传 planner 时，即使有多步 plan 也不反思。"""
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="done", finish_reason="stop")
        )

        stage, _ = _make_stage(provider=provider)  # planner=None
        ctx = _make_ctx()
        ctx.execution_plan = self._make_multistep_plan()

        result = await stage.run(ctx)

        assert provider.chat_stream_with_retry.call_count == 1
        assert result.response_text == "done"

    @pytest.mark.asyncio
    async def test_reflection_rerun_hard_capped_at_one(self):
        """should_replan 恒为 True 也最多重跑 1 轮——硬上限不可被绕过。"""
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="r1", finish_reason="stop"),
            LLMResponse(content="r2", finish_reason="stop"),
            # 第三次不应被调用；若被调用会 StopIteration 暴露回归
        ])

        planner = AsyncMock()
        planner.reflect = AsyncMock(
            return_value=self._make_feedback(should_replan=True, critique="still bad")
        )

        stage, _ = _make_stage(provider=provider, planner=planner)
        ctx = _make_ctx()
        ctx.execution_plan = self._make_multistep_plan()

        result = await stage.run(ctx)

        # 两轮工具循环，reflect 只在第一轮后调一次，第二轮后不再反思
        assert provider.chat_stream_with_retry.call_count == 2
        assert planner.reflect.call_count == 1
        assert result.response_text == "r2"

    @pytest.mark.asyncio
    async def test_reflection_exception_falls_back_to_first_pass(self):
        """reflect 抛异常时不崩溃、不重跑，返回第一轮结果。"""
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="first only", finish_reason="stop")
        )

        planner = AsyncMock()
        planner.reflect = AsyncMock(side_effect=RuntimeError("reflect boom"))

        stage, _ = _make_stage(provider=provider, planner=planner)
        ctx = _make_ctx()
        ctx.execution_plan = self._make_multistep_plan()

        result = await stage.run(ctx)

        assert result.response_text == "first only"
        assert provider.chat_stream_with_retry.call_count == 1
        planner.reflect.assert_called_once()

    @pytest.mark.asyncio
    async def test_reflection_skipped_when_plan_is_none(self):
        """execution_plan 为 None 时不反思（即便注入了 planner）。"""
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="done", finish_reason="stop")
        )

        planner = AsyncMock()
        planner.reflect = AsyncMock()

        stage, _ = _make_stage(provider=provider, planner=planner)
        ctx = _make_ctx()
        ctx.execution_plan = None

        await stage.run(ctx)

        assert not planner.reflect.called


class TestInferenceStageBudgetHalt:
    """Daily cost budget halts the loop mid-task — the plan must NOT be marked complete."""

    def _make_multistep_plan(self):
        from echo_agent.agent.planning.models import Plan, PlanStep, StrategyType
        return Plan(
            strategy=StrategyType.PLAN_EXECUTE,
            steps=[PlanStep(index=0, description="a"), PlanStep(index=1, description="b")],
            goal="multi",
        )

    @pytest.mark.asyncio
    async def test_budget_halt_marks_plan_exhausted_not_complete(self):
        from echo_agent.agent.planning.models import StepStatus
        from echo_agent.cost.budget import CostTracker

        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="should not be reached", finish_reason="stop")
        )
        stage, _ = _make_stage(provider=provider)

        # Tracker already over the daily hard cap -> enforce() raises before any call.
        tracker = CostTracker(storage=None, enabled=True, daily_budget_usd=1.0)
        tracker._spent_usd = 5.0
        stage._cost_tracker = tracker

        captured = {}

        async def _capture_update(run_id, plan, status=None):
            captured["status"] = status
            captured["plan"] = plan

        plan_run_store = MagicMock()
        plan_run_store.update = AsyncMock(side_effect=_capture_update)
        stage._plan_run_store = plan_run_store

        ctx = _make_ctx()
        ctx.execution_plan = self._make_multistep_plan()
        ctx.plan_run_id = "run_001"

        result = await stage.run(ctx)

        # Budget message surfaced to the user, loop did not exhaust iterations.
        assert "预算" in result.response_text
        # Plan persisted as exhausted, and pending steps were NOT force-completed.
        assert captured["status"] == "exhausted"
        assert all(s.status == StepStatus.PENDING for s in captured["plan"].steps)
        # The hard gate fired before any LLM call was made.
        assert provider.chat_stream_with_retry.call_count == 0

