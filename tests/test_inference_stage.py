"""Tests for InferenceStage — LLM call loop with tool execution and circuit breaking."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.models.provider import LLMResponse, ToolCallRequest
from echo_agent.tools import ToolResult


def _make_config():
    config = MagicMock()
    config.skills = MagicMock()
    config.skills.creation_nudge_interval = 0
    config.memory = MagicMock()
    config.memory.memory_nudge_interval = 0
    config.agent = MagicMock()
    config.agent.tool_concurrency = MagicMock()
    config.agent.tool_concurrency.enabled = False
    config.agent.tool_concurrency.max_concurrent = 1
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
        assert result.task_incomplete is False  # clean finish → task may be SUCCESS


class TestInferenceStagePlanProgress:
    @pytest.mark.asyncio
    async def test_tool_hint_marks_real_step_progress(self):
        from echo_agent.agent.planning.models import (
            Plan, PlanStep, StepStatus, StrategyType,
        )

        call = ToolCallRequest(id="call-1", name="exec", arguments={"command": "true"})
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="", tool_calls=[call], finish_reason="tool_calls"),
            LLMResponse(content="done", finish_reason="stop"),
        ])
        tools = MagicMock()
        tools.execute = AsyncMock(return_value=MagicMock(
            success=True, text="ok", error=None, metadata={}, is_infra_failure=False,
        ))
        tools.has = MagicMock(return_value=False)
        tools.get = MagicMock(return_value=None)
        tools.apply_spill = MagicMock(side_effect=lambda _name, _ctx, result: result)

        stage, _ = _make_stage(provider=provider, tools=tools)
        plan_store = MagicMock()
        plan_store.update = AsyncMock()
        stage._plan_run_store = plan_store
        ctx = _make_ctx(tool_defs=[{"function": {"name": "exec"}}])
        ctx.execution_plan = Plan(
            strategy=StrategyType.PLAN_EXECUTE,
            goal="run check",
            steps=[PlanStep(index=0, description="run check", tool_hint="exec")],
        )
        ctx.plan_run_id = "run-1"

        result = await stage.run(ctx)

        assert result.task_incomplete is False
        assert ctx.execution_plan.steps[0].status == StepStatus.COMPLETED
        assert ctx.execution_plan.steps[0].result == "ok"
        statuses = [call.kwargs.get("status") for call in plan_store.update.await_args_list]
        assert "running" in statuses
        assert statuses[-1] == "complete"


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
        # A provider error is NOT a finished task: it must surface so a dispatched
        # board task is written back as FAILED, not SUCCESS.
        assert result.task_incomplete is True


class TestInferenceStageApprovalDenied:
    """Tool call rejected by ApprovalGate — returns rejection message."""

    @pytest.mark.asyncio
    async def test_tool_denied(self):
        from echo_agent.tools import ToolResult

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


class TestInferenceStageOptimisticStreaming:
    """乐观流式:能就地重绘的通道逐 delta 直发,工具轮的草稿事后撤回。"""

    @staticmethod
    def _ctx_on(channel: str):
        # ChannelsConfig 真实默认值,让 _can_retract_draft 走真实判定。
        from echo_agent.config.schema import ChannelsConfig
        ctx = _make_ctx()
        ctx.event.channel = channel
        ctx.stream_publisher.discard = AsyncMock()
        return ctx, ChannelsConfig()

    @pytest.mark.asyncio
    async def test_cli_tool_turn_retracts_draft(self):
        from echo_agent.tools import ToolResult
        tc = ToolCallRequest(id="call_1", name="search", arguments={"q": "beijing"})
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="let me check", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="It is sunny.", finish_reason="stop"),
        ])
        tools = MagicMock()
        tools.execute = AsyncMock(return_value=ToolResult(success=True, output="sunny"))
        tools.has = MagicMock(return_value=False)

        stage, _ = _make_stage(provider=provider, tools=tools)
        ctx, channels = self._ctx_on("gateway:cli")
        stage._config.channels = channels

        result = await stage.run(ctx)

        # 第一轮(工具轮)的草稿被撤回,恰好一次;最终答案照常返回。
        assert ctx.stream_publisher.discard.await_count == 1
        assert result.response_text == "It is sunny."
        # 该通道用 stream 策略下发,不走 provider 缓冲。
        assert provider.chat_stream_with_retry.await_args_list[0].kwargs["draft_policy"] == "stream"

    @pytest.mark.asyncio
    async def test_cli_text_only_turn_never_retracts(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="Hello world", finish_reason="stop")
        )
        stage, _ = _make_stage(provider=provider)
        ctx, channels = self._ctx_on("gateway:cli")
        stage._config.channels = channels

        result = await stage.run(ctx)

        assert ctx.stream_publisher.discard.await_count == 0
        assert result.response_text == "Hello world"

    @pytest.mark.asyncio
    async def test_send_only_channel_uses_buffer_and_never_retracts(self):
        # webhook 无法撤回已发内容 → 必须走 buffer,且永不调 discard。
        from echo_agent.tools import ToolResult
        tc = ToolCallRequest(id="call_1", name="search", arguments={"q": "x"})
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="let me check", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="It is sunny.", finish_reason="stop"),
        ])
        tools = MagicMock()
        tools.execute = AsyncMock(return_value=ToolResult(success=True, output="sunny"))
        tools.has = MagicMock(return_value=False)

        stage, _ = _make_stage(provider=provider, tools=tools)
        ctx, channels = self._ctx_on("webhook")
        stage._config.channels = channels

        await stage.run(ctx)

        assert ctx.stream_publisher.discard.await_count == 0
        assert provider.chat_stream_with_retry.await_args_list[0].kwargs["draft_policy"] == "buffer"


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
        # Iteration ceiling means the task did not finish → incomplete.
        assert result.task_incomplete is True


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


@pytest.mark.asyncio
async def test_empty_content_no_tool_calls_gets_fallback():
    # Empty content with no tool calls breaks the loop with loop_exhausted=False,
    # so the safety net must fill a friendly fallback regardless of loop_exhausted.
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(
        return_value=LLMResponse(content=None, finish_reason="stop")
    )
    provider.get_default_model = MagicMock(return_value="gpt-5.5")
    stage, _bus = _make_stage(provider=provider)
    ctx = _make_ctx()
    result = await stage._run_tool_loop(ctx, ctx.messages)
    assert result.response_text  # non-empty fallback text
    assert "issue" in result.response_text.lower() or "try" in result.response_text.lower()



def test_loopresult_has_degraded_notices_default():
    from echo_agent.agent.pipeline.inference_stage import _LoopResult
    r = _LoopResult()
    assert r.degraded_notices == []


class TestInferenceStageLengthTruncation:
    """finish_reason='length' — 截断分层处理：保部分正文 / 降级重试 / 兜底。"""

    @pytest.mark.asyncio
    async def test_partial_content_is_automatically_continued(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="第一段。重复边界", finish_reason="length"),
            LLMResponse(content="重复边界第二段。", finish_reason="stop"),
        ])
        stage, _bus = _make_stage(provider=provider)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert result.response_text == "第一段。重复边界第二段。"
        assert result.degraded_notices == []
        assert result.output_truncated is False
        assert result.task_incomplete is False
        assert provider.chat_stream_with_retry.call_count == 2
        retry_kwargs = provider.chat_stream_with_retry.call_args_list[1].kwargs
        assert retry_kwargs["tools"] is None

    @pytest.mark.asyncio
    async def test_partial_continuation_exhaustion_keeps_notice(self):
        from echo_agent.agent.degraded_notice import notice_for, REASON_OUTPUT_TRUNCATED
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="部分答案...", finish_reason="length")
        )
        stage, _bus = _make_stage(provider=provider)
        session = _make_session()
        session.metadata = {}
        ctx = _make_ctx(session=session)

        result = await stage.run(ctx)

        assert result.response_text == "部分答案..."
        assert notice_for(REASON_OUTPUT_TRUNCATED) in result.degraded_notices
        assert result.output_truncated is True
        assert result.task_incomplete is True
        assert result.termination_reason == "output_truncated"
        assert provider.chat_stream_with_retry.call_count == 4
        assert session.metadata["_output_continuation"]["tail"] == "部分答案..."

    @pytest.mark.asyncio
    async def test_empty_truncation_retries_once_without_tools(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="", finish_reason="length"),
            LLMResponse(content="简短结论", finish_reason="stop"),
        ])
        stage, _bus = _make_stage(provider=provider)
        ctx = _make_ctx(tool_defs=[{"function": {"name": "exec"}}])

        result = await stage.run(ctx)

        assert result.response_text == "简短结论"
        assert result.output_truncated is False
        assert result.task_incomplete is False
        assert provider.chat_stream_with_retry.call_count == 2
        # The retry call must strip tools so the whole budget goes to text.
        retry_kwargs = provider.chat_stream_with_retry.call_args_list[1].kwargs
        assert retry_kwargs["tools"] is None

    @pytest.mark.asyncio
    async def test_double_truncation_gives_up_with_notice(self):
        from echo_agent.agent.degraded_notice import notice_for, REASON_OUTPUT_TRUNCATED
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="", finish_reason="length")
        )
        stage, _bus = _make_stage(provider=provider)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        # Retried once, then gave up — exactly 2 calls, truncation notice attached.
        assert provider.chat_stream_with_retry.call_count == 2
        assert notice_for(REASON_OUTPUT_TRUNCATED) in result.degraded_notices
        assert result.output_truncated is True
        assert result.task_incomplete is True

    @pytest.mark.asyncio
    async def test_empty_truncation_on_last_iteration_skips_retry(self):
        """最后一轮空截断没有剩余迭代可重试：立即放弃并附截断提示，
        不留下永远得不到回应的孤立 [系统] 重试指令。"""
        from echo_agent.agent.degraded_notice import notice_for, REASON_OUTPUT_TRUNCATED
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="", finish_reason="length")
        )
        stage, _bus = _make_stage(provider=provider, max_iterations=1)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        # No retry attempted — the range is exhausted, so exactly 1 call.
        assert provider.chat_stream_with_retry.call_count == 1
        assert notice_for(REASON_OUTPUT_TRUNCATED) in result.degraded_notices
        # The retry instruction must NOT be injected when it can never be answered.
        assert not any(
            "超出输出长度上限" in str(m.get("content", "")) for m in ctx.messages
        )


class TestArtifactOutputContract:
    @pytest.mark.asyncio
    async def test_chat_only_answer_cannot_complete_required_artifact(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="这是未写入文件的报告正文", finish_reason="stop")
        )
        stage, _bus = _make_stage(provider=provider)
        session = _make_session()
        session.metadata = {}
        ctx = _make_ctx(
            session=session,
            tool_defs=[{"function": {"name": "artifact_create"}}],
        )
        ctx.artifact_required = True

        result = await stage.run(ctx)

        assert result.task_incomplete is True
        assert result.termination_reason == "artifact_not_delivered"
        assert "未成功生成并交付" in result.response_text
        assert provider.chat_stream_with_retry.call_count == 4
        continuation = session.metadata["_artifact_continuation"]
        assert continuation["version"] == 1
        assert continuation["trace_id"] == "trace_001"
        assert continuation["source_event_id"] == "evt_001"
        assert continuation["context_key"] == "test:chat_1"

    @pytest.mark.asyncio
    async def test_ordinary_turn_clears_stale_artifact_continuation(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="ordinary answer", finish_reason="stop")
        )
        stage, _bus = _make_stage(provider=provider)
        session = _make_session()
        session.metadata = {
            "_artifact_continuation": {
                "version": 1,
                "trace_id": "old",
                "source_event_id": "evt-old",
                "context_key": "test:chat_1",
                "updated_at": 1.0,
            },
        }
        ctx = _make_ctx(session=session)

        result = await stage.run(ctx)

        assert result.response_text == "ordinary answer"
        assert "_artifact_continuation" not in session.metadata

    @pytest.mark.asyncio
    async def test_successful_delivery_satisfies_required_artifact(self):
        deliver = ToolCallRequest(
            id="deliver-1", name="artifact_deliver", arguments={"artifact_id": "a" * 32},
        )
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(return_value=LLMResponse(
            content="", tool_calls=[deliver], finish_reason="tool_calls",
        ))
        tools = MagicMock()
        tools.execute = AsyncMock(return_value=ToolResult(output='{"delivered": true}'))
        tools.has = MagicMock(return_value=False)
        tools.get = MagicMock(return_value=None)
        stage, _bus = _make_stage(provider=provider, tools=tools)
        session = _make_session()
        session.metadata = {}
        ctx = _make_ctx(
            session=session,
            tool_defs=[{"function": {"name": "artifact_deliver"}}],
        )
        ctx.artifact_required = True

        result = await stage.run(ctx)

        assert result.response_text == "报告已生成、校验并交付。"
        assert result.task_incomplete is False
        assert result.termination_reason == ""
        assert provider.chat_stream_with_retry.call_count == 1
        assert "_artifact_continuation" not in session.metadata
        tools.execute.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_truncated_artifact_prose_retries_with_tools_enabled(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="被截断的报告正文", finish_reason="length"),
            LLMResponse(content="仍然没有使用产物", finish_reason="stop"),
            LLMResponse(content="仍然没有使用产物", finish_reason="stop"),
            LLMResponse(content="仍然没有使用产物", finish_reason="stop"),
        ])
        stage, _bus = _make_stage(provider=provider)
        session = _make_session()
        session.metadata = {}
        ctx = _make_ctx(
            session=session,
            tool_defs=[{"function": {"name": "artifact_create"}}],
        )
        ctx.artifact_required = True

        result = await stage.run(ctx)

        retry_kwargs = provider.chat_stream_with_retry.call_args_list[1].kwargs
        assert retry_kwargs["tools"] == ctx.tool_defs
        assert "被截断的报告正文" not in result.response_text
        assert result.task_incomplete is True

    @pytest.mark.asyncio
    async def test_content_filter_is_incomplete_without_provider_fallback(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content=None, finish_reason="content_filter")
        )
        stage, _bus = _make_stage(provider=provider)

        result = await stage.run(_make_ctx())

        assert "内容安全策略" in result.response_text
        assert result.task_incomplete is True
        assert result.termination_reason == "content_filtered"
        assert provider.chat_stream_with_retry.call_count == 1


class TestInferenceStageFinalAnswerForcing:
    """最后一轮强制收敛：去掉 tools 逼模型给结论，而不是白跑整轮。"""

    @pytest.mark.asyncio
    async def test_last_iteration_strips_tools_and_gets_conclusion(self):
        from echo_agent.agent.degraded_notice import notice_for, REASON_LOOP_EXHAUSTED
        tc = ToolCallRequest(id="call_1", name="tool_a", arguments={"x": "1"})

        provider = AsyncMock()

        def _respond(**kwargs):
            if kwargs.get("tools"):
                return LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls")
            return LLMResponse(content="目前进展:已完成 A,缺 B。", finish_reason="stop")

        provider.chat_stream_with_retry = AsyncMock(side_effect=lambda **kw: _respond(**kw))

        tools_reg = MagicMock()
        tool_result = MagicMock(success=True, text="ok", error=None, metadata={})
        tools_reg.execute = AsyncMock(return_value=tool_result)
        tools_reg.has = MagicMock(return_value=False)

        stage, _bus = _make_stage(provider=provider, tools=tools_reg, max_iterations=3)
        ctx = _make_ctx(tool_defs=[{"function": {"name": "tool_a"}}])

        result = await stage.run(ctx)

        # The forced final call produced a real conclusion, not the fallback.
        assert result.response_text == "目前进展:已完成 A,缺 B。"
        assert notice_for(REASON_LOOP_EXHAUSTED) in result.degraded_notices
        # Last call had tools stripped.
        last_kwargs = provider.chat_stream_with_retry.call_args_list[-1].kwargs
        assert last_kwargs["tools"] is None
        # A convergence instruction was injected for the final call.
        assert any(
            m.get("role") == "user" and "工具调用上限" in str(m.get("content", ""))
            for m in ctx.messages
        )

    @pytest.mark.asyncio
    async def test_forced_convergence_keeps_plan_resumable(self):
        """强制收敛产出的是"答案"而非"任务完成"：plan 不得被标 complete，
        plan_run 必须存成 exhausted，pending 步骤保持 pending 以便续跑。"""
        from echo_agent.agent.planning.models import Plan, PlanStep, StepStatus, StrategyType

        tc = ToolCallRequest(id="call_1", name="tool_a", arguments={"x": "1"})
        provider = AsyncMock()

        def _respond(**kwargs):
            if kwargs.get("tools"):
                return LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls")
            return LLMResponse(content="目前进展:已完成 A,缺 B。", finish_reason="stop")

        provider.chat_stream_with_retry = AsyncMock(side_effect=lambda **kw: _respond(**kw))

        tools_reg = MagicMock()
        tools_reg.execute = AsyncMock(return_value=MagicMock(success=True, text="ok", error=None, metadata={}))
        tools_reg.has = MagicMock(return_value=False)

        stage, _bus = _make_stage(provider=provider, tools=tools_reg, max_iterations=3)

        captured = {}

        async def _capture_update(run_id, plan, status=None):
            captured["status"] = status
            captured["plan"] = plan

        plan_run_store = MagicMock()
        plan_run_store.update = AsyncMock(side_effect=_capture_update)
        stage._plan_run_store = plan_run_store

        ctx = _make_ctx(tool_defs=[{"function": {"name": "tool_a"}}])
        ctx.execution_plan = Plan(
            strategy=StrategyType.PLAN_EXECUTE,
            steps=[PlanStep(index=0, description="a"), PlanStep(index=1, description="b")],
            goal="multi",
        )
        ctx.plan_run_id = "run_001"

        result = await stage.run(ctx)

        # The answer was delivered, but the task is unfinished:
        assert result.response_text == "目前进展:已完成 A,缺 B。"
        assert not ctx.execution_plan.is_complete
        assert captured["status"] == "exhausted"
        assert all(s.status == StepStatus.PENDING for s in captured["plan"].steps)

    @pytest.mark.asyncio
    async def test_forced_final_empty_reply_no_progress_notice(self):
        """强制收敛调用没产出正文时，不能附加"以上是目前的进展"——
        用户根本没看到任何进展，该提示与事实不符。"""
        from echo_agent.agent.degraded_notice import notice_for, REASON_LOOP_EXHAUSTED

        tc = ToolCallRequest(id="call_1", name="tool_a", arguments={"x": "1"})
        provider = AsyncMock()

        def _respond(**kwargs):
            if kwargs.get("tools"):
                return LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls")
            return LLMResponse(content="", finish_reason="stop")

        provider.chat_stream_with_retry = AsyncMock(side_effect=lambda **kw: _respond(**kw))

        tools_reg = MagicMock()
        tools_reg.execute = AsyncMock(return_value=MagicMock(success=True, text="ok", error=None, metadata={}))
        tools_reg.has = MagicMock(return_value=False)

        stage, _bus = _make_stage(provider=provider, tools=tools_reg, max_iterations=3)
        ctx = _make_ctx(tool_defs=[{"function": {"name": "tool_a"}}])

        result = await stage.run(ctx)

        assert notice_for(REASON_LOOP_EXHAUSTED) not in result.degraded_notices
        # The generic safety-net fallback still guarantees a reply.
        assert result.response_text

    @pytest.mark.asyncio
    async def test_forced_final_truncated_partial_gets_both_notices(self):
        """强制收敛调用自身被截断但有部分正文：截断提示 + 进展提示都要有。"""
        from echo_agent.agent.degraded_notice import (
            notice_for,
            REASON_LOOP_EXHAUSTED,
            REASON_OUTPUT_TRUNCATED,
        )

        tc = ToolCallRequest(id="call_1", name="tool_a", arguments={"x": "1"})
        provider = AsyncMock()

        def _respond(**kwargs):
            if kwargs.get("tools"):
                return LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls")
            return LLMResponse(content="部分进展...", finish_reason="length")

        provider.chat_stream_with_retry = AsyncMock(side_effect=lambda **kw: _respond(**kw))

        tools_reg = MagicMock()
        tools_reg.execute = AsyncMock(return_value=MagicMock(success=True, text="ok", error=None, metadata={}))
        tools_reg.has = MagicMock(return_value=False)

        stage, _bus = _make_stage(provider=provider, tools=tools_reg, max_iterations=3)
        ctx = _make_ctx(tool_defs=[{"function": {"name": "tool_a"}}])

        result = await stage.run(ctx)

        assert result.response_text == "部分进展..."
        assert notice_for(REASON_OUTPUT_TRUNCATED) in result.degraded_notices
        assert notice_for(REASON_LOOP_EXHAUSTED) in result.degraded_notices


def test_inferenceresult_has_degraded_notices_default():
    from echo_agent.agent.pipeline.types import InferenceResult
    r = InferenceResult()
    assert r.degraded_notices == []


@pytest.mark.asyncio
async def test_approval_denial_with_notify_collects_notice():
    # When an approval denial carries notify_user + notice, the loop must
    # collect that notice into degraded_notices and bubble it to the result.
    from echo_agent.tools import ToolResult

    tc = ToolCallRequest(id="call_1", name="exec", arguments={"command": "rm -rf /"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(side_effect=[
        LLMResponse(content="Let me run that", tool_calls=[tc], finish_reason="tool_calls"),
        LLMResponse(content="Understood, denied.", finish_reason="stop"),
    ])

    denial = ToolResult(success=False, error="approval unavailable")
    gate = AsyncMock()
    gate.check = AsyncMock(return_value=MagicMock(
        denial=denial, approved_actions=frozenset(),
        notify_user=True, notice="⚠️ 安全审批暂时不可用,已暂停。",
    ))

    stage, _bus = _make_stage(provider=provider, approval_gate=gate)
    ctx = _make_ctx()

    result = await stage.run(ctx)

    assert result.degraded_notices == ["⚠️ 安全审批暂时不可用,已暂停。"]


@pytest.mark.asyncio
async def test_terminal_approval_denial_does_not_ask_llm_to_rephrase():
    """Timeout/delivery failures have deterministic user copy already.

    Feeding them back through a second LLM round caused the misleading Weixin
    "reply 1/2/3" instructions, so a terminal denial must end tool inference.
    """
    from echo_agent.tools import ToolResult

    tc = ToolCallRequest(id="call_1", name="cronjob", arguments={"action": "add"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(return_value=LLMResponse(
        content="Let me create that schedule.",
        tool_calls=[tc],
        finish_reason="tool_calls",
    ))

    notice = "⚠️ 审批等待已超时,请重新发起。"
    gate = AsyncMock()
    gate.check = AsyncMock(return_value=MagicMock(
        denial=ToolResult(success=False, error="approval timed out"),
        approved_actions=frozenset(),
        notify_user=True,
        notice=notice,
        terminal=True,
    ))

    stage, _bus = _make_stage(provider=provider, approval_gate=gate)
    result = await stage.run(_make_ctx())

    assert provider.chat_stream_with_retry.call_count == 1
    assert result.degraded_notices == [notice]
    assert "Let me create" not in result.response_text


@pytest.mark.asyncio
async def test_repeat_blocked_collects_notice():
    # A repeat-blocked tool call must collect the repeat-blocked notice.
    from echo_agent.agent.degraded_notice import REASON_REPEAT_BLOCKED, notice_for

    tc = ToolCallRequest(id="call_1", name="search", arguments={"q": "test"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(side_effect=[
        LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
        LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
        LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
        LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
        LLMResponse(content="Done after block", finish_reason="stop"),
    ])

    tools_reg = MagicMock()
    tools_reg.execute = AsyncMock(return_value=MagicMock(success=True, text="result", error=None, metadata={}))
    tools_reg.has = MagicMock(return_value=False)

    stage, _bus = _make_stage(provider=provider, tools=tools_reg)
    ctx = _make_ctx()

    result = await stage.run(ctx)

    assert notice_for(REASON_REPEAT_BLOCKED) in result.degraded_notices


@pytest.mark.asyncio
async def test_approval_denial_without_notify_collects_nothing():
    # A denial without notify_user must NOT leak a notice (notice path gated).
    from echo_agent.tools import ToolResult

    tc = ToolCallRequest(id="call_1", name="exec", arguments={"command": "ls"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(side_effect=[
        LLMResponse(content="run", tool_calls=[tc], finish_reason="tool_calls"),
        LLMResponse(content="ok", finish_reason="stop"),
    ])

    denial = ToolResult(success=False, error="denied")
    gate = AsyncMock()
    gate.check = AsyncMock(return_value=MagicMock(
        denial=denial, approved_actions=frozenset(),
        notify_user=False, notice="",
    ))

    stage, _bus = _make_stage(provider=provider, approval_gate=gate)
    ctx = _make_ctx()

    result = await stage.run(ctx)

    assert result.degraded_notices == []


def test_processresult_has_degraded_notices_default():
    from echo_agent.agent.pipeline.response_stage import ProcessResult
    r = ProcessResult()
    assert r.degraded_notices == []


# ── 工具执行中断兜底（except BaseException, 行 528-553）────────────────────


@pytest.mark.asyncio
async def test_tool_execution_exception_appends_interrupted_message_and_reraises():
    """工具执行抛异常时：在未产出 tool 消息的情况下，必须补一条
    'interrupted' tool 消息（否则下一轮 LLM 会因 tool_calls 无配对而拒绝），
    记一次熔断 failure，然后把异常重新抛出。"""
    tc = ToolCallRequest(id="call_x", name="exec", arguments={"command": "boom"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(
        return_value=LLMResponse(content="run it", tool_calls=[tc], finish_reason="tool_calls")
    )

    tools_reg = MagicMock()
    tools_reg.execute = AsyncMock(side_effect=RuntimeError("tool blew up"))
    tools_reg.has = MagicMock(return_value=False)

    stage, _bus = _make_stage(provider=provider, tools=tools_reg)
    ctx = _make_ctx()

    with pytest.raises(RuntimeError, match="tool blew up"):
        await stage._run_tool_loop(ctx, ctx.messages)

    # 补偿的 interrupted tool 消息已配对到 tool_call_id
    tool_msgs = [m for m in ctx.messages if m.get("role") == "tool"]
    assert any(
        m["tool_call_id"] == "call_x" and "interrupted" in m["content"].lower()
        for m in tool_msgs
    )
    # 中断被计入熔断 failure（而非静默跳过）
    stage._circuit_breaker.record_failure.assert_called_with("exec")


@pytest.mark.asyncio
async def test_tool_exception_after_message_appended_skips_compensation():
    """若异常发生在 tool 消息已 append 之后（这里用 record_success 抛错模拟
    后置阶段异常）：兜底块的补偿分支不应再追加第二条 tool 消息，但异常仍冒泡。"""
    tc = ToolCallRequest(id="call_y", name="exec", arguments={"command": "ok"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(
        return_value=LLMResponse(content="run", tool_calls=[tc], finish_reason="tool_calls")
    )

    tools_reg = MagicMock()
    tools_reg.execute = AsyncMock(
        return_value=MagicMock(success=True, text="result", error=None, metadata={})
    )
    tools_reg.has = MagicMock(return_value=False)

    stage, _bus = _make_stage(provider=provider, tools=tools_reg)
    # tool 消息 append 之后才触发异常
    stage._circuit_breaker.record_success = MagicMock(side_effect=RuntimeError("post fail"))
    ctx = _make_ctx()

    with pytest.raises(RuntimeError, match="post fail"):
        await stage._run_tool_loop(ctx, ctx.messages)

    # 只有一条 call_y 的 tool 消息（成功结果），没有额外的 interrupted 补偿消息
    call_y_msgs = [
        m for m in ctx.messages
        if m.get("role") == "tool" and m.get("tool_call_id") == "call_y"
    ]
    assert len(call_y_msgs) == 1
    assert "interrupted" not in call_y_msgs[0]["content"].lower()


@pytest.mark.asyncio
async def test_multi_tool_batch_fail_fast_skips_later_serial_tools():
    """同一批 response.tool_calls 含两个工具：第一个执行抛异常时，必须
    fail-fast —— 第二个工具的 execute 根本不被调用（其副作用不发生），
    异常向上冒泡，且第一个工具拿到配对的 interrupted tool 消息。"""
    tc1 = ToolCallRequest(id="call_a", name="exec", arguments={"command": "boom"})
    tc2 = ToolCallRequest(id="call_b", name="writer", arguments={"path": "out.txt"})
    provider = AsyncMock()
    provider.chat_stream_with_retry = AsyncMock(
        return_value=LLMResponse(
            content="run both", tool_calls=[tc1, tc2], finish_reason="tool_calls"
        )
    )

    # First tool blows up; second tool is a spy that must never be reached.
    async def _execute(name, args, exec_ctx):
        if name == "exec":
            raise RuntimeError("first tool blew up")
        return MagicMock(success=True, text="written", error=None, metadata={})

    tools_reg = MagicMock()
    tools_reg.execute = AsyncMock(side_effect=_execute)
    tools_reg.has = MagicMock(return_value=False)

    stage, _bus = _make_stage(provider=provider, tools=tools_reg)
    ctx = _make_ctx()

    with pytest.raises(RuntimeError, match="first tool blew up"):
        await stage._run_tool_loop(ctx, ctx.messages)

    # Fail-fast: the second (serial) tool's execute was never called.
    called_names = [c.args[0] for c in tools_reg.execute.await_args_list]
    assert "writer" not in called_names
    assert called_names == ["exec"]

    # First tool got its paired interrupted tool message.
    tool_msgs = [m for m in ctx.messages if m.get("role") == "tool"]
    assert any(
        m["tool_call_id"] == "call_a" and "interrupted" in m["content"].lower()
        for m in tool_msgs
    )
    # The unexecuted second tool produced no tool message before the raise.
    assert not any(m["tool_call_id"] == "call_b" for m in tool_msgs)


class TestInferenceStageInterrupt:
    """Cooperative interrupt checkpoints in the tool loop. A flagged session
    stops cleanly at an iteration boundary, keeping partial text and marking the
    result as user-stopped (not exhausted)."""

    @pytest.mark.asyncio
    async def test_checkpoint_at_loop_top_stops_before_any_llm_call(self):
        from echo_agent.agent.interrupt_manager import InterruptManager

        # Interrupt set BEFORE the loop starts → the top-of-loop checkpoint fires
        # on iteration 0, so the provider is never called.
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="should not be reached", finish_reason="stop")
        )
        stage, bus = _make_stage(provider=provider)
        im = InterruptManager()
        im.request("test:chat_1")
        im.interrupt("test:chat_1")
        stage._interrupt = im

        ctx = _make_ctx()
        result = await stage.run(ctx)

        assert "已按你的请求停止" in result.response_text
        provider.chat_stream_with_retry.assert_not_called()

    @pytest.mark.asyncio
    async def test_checkpoint_after_llm_stops_before_running_tools(self):
        from echo_agent.agent.interrupt_manager import InterruptManager

        # Not interrupted at loop top; the LLM call flags the session (simulating
        # an interrupt arriving DURING a long call) and returns tool calls. The
        # second checkpoint must break before executing the batch.
        im = InterruptManager()
        im.request("test:chat_1")

        tc = ToolCallRequest(id="call_1", name="exec", arguments={"command": "sleep 60"})

        async def _fake_llm(*args, **kwargs):
            im.interrupt("test:chat_1")     # interrupt lands during the call
            return LLMResponse(content="正在处理", tool_calls=[tc], finish_reason="tool_calls")

        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(side_effect=_fake_llm)

        tools_reg = MagicMock()
        tools_reg.execute = AsyncMock()
        tools_reg.has = MagicMock(return_value=False)

        stage, bus = _make_stage(provider=provider, tools=tools_reg)
        stage._interrupt = im

        ctx = _make_ctx()
        result = await stage.run(ctx)

        assert "已按你的请求停止" in result.response_text
        # The pending tool batch was NOT executed.
        tools_reg.execute.assert_not_called()
        assert result.total_tool_calls == 0

    @pytest.mark.asyncio
    async def test_no_interrupt_runs_normally(self):
        # Guard: an un-flagged session (or no manager) must not trip the
        # checkpoint — normal completion.
        from echo_agent.agent.interrupt_manager import InterruptManager

        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="正常完成", finish_reason="stop")
        )
        stage, bus = _make_stage(provider=provider)
        im = InterruptManager()
        im.request("test:chat_1")           # running but NOT interrupted
        stage._interrupt = im

        ctx = _make_ctx()
        result = await stage.run(ctx)

        assert result.response_text == "正常完成"
        assert "已按你的请求停止" not in result.response_text
