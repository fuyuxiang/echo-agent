"""Concurrency path of InferenceStage._execute_tool_batch."""
from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.models.provider import LLMResponse, ToolCallRequest


def _cfg(enabled=True, max_concurrent=4):
    c = MagicMock()
    c.skills.creation_nudge_interval = 0
    c.memory.memory_nudge_interval = 0
    c.agent.tool_concurrency.enabled = enabled
    c.agent.tool_concurrency.max_concurrent = max_concurrent
    return c


def _stage(tools, enabled=True, max_concurrent=4):
    bus = AsyncMock()
    bus.publish_outbound = AsyncMock()
    gate = AsyncMock()
    gate.check = AsyncMock(return_value=MagicMock(denial=None, approved_actions=frozenset()))
    tracer = MagicMock()
    tracer.start_span = MagicMock(return_value="s")
    tracer.end_span = MagicMock()
    tele = MagicMock()
    tele.available = False
    ic = MagicMock()
    ic.validate_response = MagicMock(return_value=[])
    cb = MagicMock()
    creds = MagicMock()
    creds.get_for_tool = MagicMock(return_value={})
    prov = AsyncMock()
    return InferenceStage(
        config=_cfg(enabled, max_concurrent), bus=bus, provider=prov, router=None,
        tools=tools, approval_gate=gate, credentials=creds, tracer=tracer,
        telemetry=tele, inference=ic, circuit_breaker=cb,
        default_model="m", max_iterations=10,
    )


def _ctx():
    ev = MagicMock()
    ev.channel = "t"
    ev.chat_id = "c"
    ev.reply_to_id = None
    ev.event_id = "e"
    ev.metadata = {}
    ev.session_key = "t:c"
    ev.sender_id = "u"
    ev.text = "hi"
    ctx = PipelineContext(event=ev, session=MagicMock(), trace_id="tr", publish_response=True)
    ctx.messages = [{"role": "user", "content": "hi"}]
    ctx.tool_defs = []
    ctx.stream_publisher = MagicMock()
    ctx.stream_publisher.on_delta = AsyncMock()
    ctx.execution_plan = None
    ctx.task_type = "chat"
    return ctx


def _tool(read_only=True):
    t = MagicMock()
    t.execution_mode = MagicMock(return_value="read_only" if read_only else "side_effect")
    return t


def _two_reads_then_stop():
    """Provider: turn 1 returns two read_file tool calls, turn 2 stops."""
    calls = [
        LLMResponse(content="", finish_reason="tool_calls", tool_calls=[
            ToolCallRequest(id="a", name="read_file", arguments={"path": "x.txt"}),
            ToolCallRequest(id="b", name="read_file", arguments={"path": "y.txt"}),
        ]),
        LLMResponse(content="done", finish_reason="stop"),
    ]
    it = iter(calls)
    return AsyncMock(side_effect=lambda **kw: next(it))


@pytest.mark.asyncio
async def test_concurrent_reads_preserve_message_order():
    reg = MagicMock()
    reg.has = MagicMock(return_value=False)
    reg.get = MagicMock(return_value=_tool(read_only=True))
    reg.execute = AsyncMock(side_effect=lambda name, args, ctx: MagicMock(
        success=True, text=f"read:{args['path']}", error=None))
    stage = _stage(reg)
    stage._provider.chat_stream_with_retry = _two_reads_then_stop()
    ctx = _ctx()
    result = await stage._run_tool_loop(ctx, ctx.messages)
    # both tool messages present, in original order a then b
    tool_msgs = [m for m in ctx.messages if m.get("role") == "tool"]
    assert [m["tool_call_id"] for m in tool_msgs] == ["a", "b"]
    assert result.total_tool_calls == 2


@pytest.mark.asyncio
async def test_concurrent_failure_isolated():
    def _exec(name, args, ctx):
        if args["path"] == "x.txt":
            raise RuntimeError("boom")
        return MagicMock(success=True, text="ok", error=None)
    reg = MagicMock()
    reg.has = MagicMock(return_value=False)
    reg.get = MagicMock(return_value=_tool(read_only=True))
    reg.execute = AsyncMock(side_effect=_exec)
    stage = _stage(reg)
    stage._provider.chat_stream_with_retry = _two_reads_then_stop()
    ctx = _ctx()
    await stage._run_tool_loop(ctx, ctx.messages)
    tool_msgs = [m for m in ctx.messages if m.get("role") == "tool"]
    # failed tool gets interrupted message, sibling still succeeds; both paired
    assert len(tool_msgs) == 2
    assert any("interrupted" in m["content"] for m in tool_msgs)
    assert any("ok" == m["content"] for m in tool_msgs)
    stage._circuit_breaker.record_failure.assert_any_call("read_file")


@pytest.mark.asyncio
async def test_concurrent_actually_parallel():
    started = []
    async def _exec(name, args, ctx):
        started.append(args["path"])
        await asyncio.sleep(0.1)
        return MagicMock(success=True, text="ok", error=None)
    reg = MagicMock()
    reg.has = MagicMock(return_value=False)
    reg.get = MagicMock(return_value=_tool(read_only=True))
    reg.execute = AsyncMock(side_effect=_exec)
    stage = _stage(reg, max_concurrent=4)
    stage._provider.chat_stream_with_retry = _two_reads_then_stop()
    ctx = _ctx()
    loop = asyncio.get_event_loop()
    t0 = loop.time()
    await stage._run_tool_loop(ctx, ctx.messages)
    elapsed = loop.time() - t0
    # two 0.1s tools concurrently ≈ 0.1s, not 0.2s
    assert elapsed < 0.18


@pytest.mark.asyncio
async def test_max_concurrent_one_is_serial():
    reg = MagicMock()
    reg.has = MagicMock(return_value=False)
    reg.get = MagicMock(return_value=_tool(read_only=True))
    reg.execute = AsyncMock(side_effect=lambda name, args, ctx: MagicMock(
        success=True, text=f"read:{args['path']}", error=None))
    stage = _stage(reg, enabled=True, max_concurrent=1)
    stage._provider.chat_stream_with_retry = _two_reads_then_stop()
    ctx = _ctx()
    result = await stage._run_tool_loop(ctx, ctx.messages)
    tool_msgs = [m for m in ctx.messages if m.get("role") == "tool"]
    assert [m["tool_call_id"] for m in tool_msgs] == ["a", "b"]
    assert result.total_tool_calls == 2


@pytest.mark.asyncio
async def test_concurrent_group_fires_post_tool_call_hook_per_tool():
    """Each concurrently-executed tool must trigger the post_tool_call hook,
    mirroring the serial group's behavior (regression for the hook being
    skipped in the concurrent _run_one path)."""
    reg = MagicMock()
    reg.has = MagicMock(return_value=False)
    reg.get = MagicMock(return_value=_tool(read_only=True))
    reg.execute = AsyncMock(side_effect=lambda name, args, ctx: MagicMock(
        success=True, text=f"read:{args['path']}", error=None))
    stage = _stage(reg, enabled=True, max_concurrent=4)
    stage._provider.chat_stream_with_retry = _two_reads_then_stop()

    hooks = MagicMock()
    hooks.has_hooks = MagicMock(side_effect=lambda name: name == "post_tool_call")
    # dispatch_modify returns the result unchanged (identity), recording calls
    hooks.dispatch_modify = AsyncMock(side_effect=lambda event, result, *a: result)
    stage._hook_registry = hooks

    ctx = _ctx()
    result = await stage._run_tool_loop(ctx, ctx.messages)

    # both reads went concurrent and each fired the post_tool_call hook
    assert result.total_tool_calls == 2
    post_calls = [c for c in hooks.dispatch_modify.await_args_list
                  if c.args[0] == "post_tool_call"]
    assert len(post_calls) == 2
    hooked_tools = {c.args[2] for c in post_calls}
    assert hooked_tools == {"read_file"}
