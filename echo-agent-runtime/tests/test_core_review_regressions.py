"""Isolated regressions from the Runtime core execution review."""

from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from datetime import datetime, timedelta, timezone
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.multi_agent.models import WorkerResult
from echo_agent.agent.multi_agent.runtime import WorkerExecutor
from echo_agent.agent.multi_agent.registry import WorkerRegistry
from echo_agent.agent.tools.delegate import DelegateTool, SpawnTool
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest
from echo_agent.models.router import HealthStatus, ModelRouter
from echo_agent.config.schema import ModelsConfig, ProviderConfig
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult


class _CounterTool(Tool):
    name = "counter"
    parameters = {"type": "object", "properties": {}}

    def __init__(self):
        self.calls = 0
        self.started = asyncio.Event()
        self.release = asyncio.Event()

    async def execute(self, params, ctx=None):
        self.calls += 1
        self.started.set()
        await self.release.wait()
        return ToolResult(output="done")


@pytest.mark.asyncio
async def test_concurrent_same_replay_key_executes_side_effect_once():
    registry = ToolRegistry()
    tool = _CounterTool()
    registry.register(tool)
    ctx = ToolExecutionContext(idempotency_key="same-operation")
    first = asyncio.create_task(registry.execute(tool.name, {}, ctx))
    await tool.started.wait()
    second = asyncio.create_task(registry.execute(tool.name, {}, ctx))
    # Let the second caller reach the registry while the first owns the effect.
    await asyncio.sleep(0)
    tool.release.set()
    results = await asyncio.gather(first, second)
    assert tool.calls == 1
    assert sum(result.success for result in results) == 1
    assert "Replay prevented" in results[1].error


@pytest.mark.asyncio
async def test_cancelled_replay_claim_does_not_leave_operation_locked():
    registry = ToolRegistry()
    tool = _CounterTool()
    registry.register(tool)
    ctx = ToolExecutionContext(idempotency_key="cancelled-operation")
    first = asyncio.create_task(registry.execute(tool.name, {}, ctx))
    await tool.started.wait()
    first.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first
    tool.release.set()
    result = await registry.execute(tool.name, {}, ctx)
    assert result.success
    assert tool.calls == 2


def _worker_registry():
    registry = MagicMock()
    registry.ready_tool_names = ["read_file", "exec", "delegate_task", "spawn_task"]
    registry.get_ready_definitions.return_value = [
        {"type": "function", "function": {"name": name}}
        for name in registry.ready_tool_names
    ]
    return registry


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["delegate", "spawn"])
async def test_workers_inherit_parent_tool_allowlist(kind):
    registry = _worker_registry()
    if kind == "delegate":
        tool = DelegateTool(
            provider=AsyncMock(), tool_registry=registry,
            worker_registry=WorkerRegistry([]), approval_gate=AsyncMock(),
            credentials=None,
        )
    else:
        tool = SpawnTool(
            provider=AsyncMock(), tool_registry=registry,
            approval_gate=AsyncMock(), credentials=None,
        )
    tool._executor.run = AsyncMock(return_value=WorkerResult(
        task_index=0, status="completed", output="done",
    ))
    ctx = ToolExecutionContext(allowed_tools=frozenset({tool.name, "read_file"}))
    if kind == "delegate":
        result = await tool.execute({"goal": "inspect", "tools": ["exec", "read_file"]}, ctx)
        assert result.success
    else:
        await tool._execute_worker("background-test", "inspect", "", ctx)
    kwargs = tool._executor.run.await_args.kwargs
    offered = {entry["function"]["name"] for entry in kwargs["tool_defs"]}
    assert offered == {"read_file"}
    # A hallucinated name must also be rejected by the execution boundary.
    blocked = await kwargs["tool_executor"]("exec", MagicMock(arguments={}), 0)
    assert not blocked.success


class _Provider(LLMProvider):
    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok")

    def get_default_model(self):
        return "test-model"


@pytest.mark.asyncio
async def test_inference_does_not_bypass_router_cooldown():
    from echo_agent.agent.pipeline.inference_stage import InferenceStage

    provider = _Provider()
    provider.chat_stream_with_retry = AsyncMock(return_value=LLMResponse(content="ok"))
    router = ModelRouter(ModelsConfig(
        default_model="test-model", providers=[ProviderConfig(name="primary")],
    ))
    router.register_provider("primary", provider)
    for _ in range(3):
        router.mark_failure("primary", "unavailable")
    stage = object.__new__(InferenceStage)
    stage._router, stage._provider, stage._default_model = router, provider, "test-model"
    response, _ = await stage._chat_stream_with_routing(
        messages=[{"role": "user", "content": "hello"}], tools=None,
        on_delta=None, task_type="chat", content="hello",
    )
    assert response.finish_reason == "error"
    provider.chat_stream_with_retry.assert_not_awaited()


@pytest.mark.asyncio
async def test_unused_fallback_does_not_consume_half_open_probes():
    from echo_agent.agent.pipeline.inference_stage import InferenceStage

    primary, backup = _Provider(), _Provider()
    primary.chat_stream_with_retry = AsyncMock(return_value=LLMResponse(content="ok"))
    backup.chat_stream_with_retry = AsyncMock(return_value=LLMResponse(content="backup"))
    router = ModelRouter(ModelsConfig(
        default_model="test-model", fallback_model="backup-model",
        providers=[
            ProviderConfig(name="primary", models=["test-model"]),
            ProviderConfig(name="backup", models=["backup-model"]),
        ],
    ))
    router.register_provider("primary", primary)
    router.register_provider("backup", backup)
    for _ in range(3):
        router.mark_failure("backup", "unavailable")
    router._health["backup"].cooldown_until = datetime.now(timezone.utc) - timedelta(seconds=1)
    stage = object.__new__(InferenceStage)
    stage._router, stage._provider, stage._default_model = router, primary, "test-model"
    for _ in range(3):
        response, _ = await stage._chat_stream_with_routing(
            messages=[{"role": "user", "content": "hello"}], tools=None,
            on_delta=None, task_type="chat", content="hello",
        )
        assert response.content == "ok"
    backup.chat_stream_with_retry.assert_not_awaited()
    health = router._health["backup"]
    assert health.half_open_allowance == health.HALF_OPEN_MAX_PROBES


@pytest.mark.asyncio
async def test_half_open_bounds_actual_calls_and_returns_cancelled_probe_slots():
    from echo_agent.agent.pipeline.inference_stage import InferenceStage

    provider = _Provider()
    both_started = asyncio.Event()
    calls = 0

    async def pending_chat(**kwargs):
        nonlocal calls
        calls += 1
        if calls == 2:
            both_started.set()
        await asyncio.Future()

    provider.chat_stream_with_retry = pending_chat
    router = ModelRouter(ModelsConfig(
        default_model="test-model", providers=[ProviderConfig(name="primary")],
    ))
    router.register_provider("primary", provider)
    health = router._health["primary"]
    health.status, health.half_open_allowance = HealthStatus.HALF_OPEN, 2
    stage = object.__new__(InferenceStage)
    stage._router, stage._provider, stage._default_model = router, provider, "test-model"

    async def request():
        return await stage._chat_stream_with_routing(
            messages=[{"role": "user", "content": "hello"}], tools=None,
            on_delta=None, task_type="chat", content="hello",
        )

    running = [asyncio.create_task(request()) for _ in range(2)]
    try:
        await asyncio.wait_for(both_started.wait(), timeout=2)
        response, _ = await request()
        assert response.finish_reason == "error"
        assert calls == 2
    finally:
        for task in running:
            task.cancel()
        await asyncio.gather(*running, return_exceptions=True)
    assert health.half_open_allowance == 2


def test_restarted_half_open_provider_can_recover(tmp_path):
    health_file = tmp_path / "model-health.json"
    health_file.write_text(json.dumps({"primary": {
        "status": "half_open", "failure_count": 3, "cooldown_until": None,
    }}), encoding="utf-8")
    router = ModelRouter(ModelsConfig(
        default_model="test-model", providers=[ProviderConfig(name="primary")],
    ), health_file=health_file)
    router.register_provider("primary", _Provider())
    assert router.route_candidates()
    assert router.admit_provider("primary")
    assert router.admit_provider("primary")
    assert not router.admit_provider("primary")


@pytest.mark.asyncio
async def test_worker_history_keeps_goal_and_complete_tool_groups():
    provider = _Provider()
    calls = 0
    goal = "Read reports and preserve this original task constraint."

    async def chat(**kwargs):
        nonlocal calls
        calls += 1
        if calls <= 3:
            return LLMResponse(finish_reason="tool_calls", tool_calls=[
                ToolCallRequest(id=f"call-{calls}-{index}", name="read_file", arguments={"path": str(index)})
                for index in range(3)
            ])
        messages = kwargs["messages"]
        assert messages[1] == {"role": "user", "content": goal}
        pending = set()
        for message in messages:
            if message["role"] == "assistant":
                assert not pending
                pending = {entry["id"] for entry in message.get("tool_calls", [])}
            elif message["role"] == "tool":
                assert message["tool_call_id"] in pending
                pending.remove(message["tool_call_id"])
        assert not pending
        return LLMResponse(content="done")

    provider.chat_with_retry = chat
    executor = WorkerExecutor(provider=provider)
    executor._MAX_MESSAGES = 8
    result = await executor.run(
        task_index=0, goal=goal, tool_defs=[],
        tool_executor=AsyncMock(return_value="content"), max_iterations=4,
    )
    assert result.status == "completed", result.error


@pytest.mark.parametrize("damage", ["null_chunk", "bad_terms", "bad_count", "bad_metadata", "bad_manifest"])
def test_structurally_corrupt_knowledge_index_rebuilds_from_source(tmp_path, damage):
    from echo_agent.knowledge import KnowledgeIndex

    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "handbook.md").write_text("# Handbook\n\nRuntime recovery is supported.", encoding="utf-8")
    index = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="index.json")
    index.rebuild()
    payload = json.loads(index.index_path.read_text(encoding="utf-8"))
    if damage == "null_chunk":
        payload["chunks"] = [None]
    elif damage == "bad_terms":
        payload["chunks"][0]["terms"] = []
    elif damage == "bad_count":
        payload["chunks"][0]["terms"] = {"runtime": "invalid"}
    elif damage == "bad_metadata":
        payload["chunks"][0]["metadata"] = []
    else:
        payload["manifest"]["docs/handbook.md"] = 7
    index.index_path.write_text(json.dumps(payload), encoding="utf-8")
    restarted = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="index.json")
    restarted.ensure_ready()
    results = restarted.search("Runtime recovery")
    assert len(results) == 1
    assert results[0].path == "docs/handbook.md"


@pytest.mark.asyncio
async def test_cancelled_dispatch_waiter_does_not_poison_future_scans():
    from echo_agent.tasks.dispatcher import TaskDispatcher

    dispatcher = TaskDispatcher(AsyncMock(), AsyncMock(), max_concurrent=1)
    await dispatcher._sem.acquire()
    dispatcher._inflight.add("waiting-task")
    waiting = asyncio.create_task(dispatcher._dispatch(SimpleNamespace(id="waiting-task")))
    await asyncio.sleep(0)
    waiting.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiting
    assert "waiting-task" not in dispatcher._inflight
    # Cancelling a waiter must not release somebody else's capacity.
    assert dispatcher._sem.locked()
    dispatcher._sem.release()


@pytest.mark.asyncio
async def test_stop_cleans_dispatches_cancelled_before_they_start():
    from echo_agent.tasks.dispatcher import TaskDispatcher

    tasks = AsyncMock()
    tasks.list_by_status.return_value = [SimpleNamespace(id="unstarted-task")]
    dispatcher = TaskDispatcher(AsyncMock(), tasks, stop_grace_sec=0)
    await dispatcher._scan_once()
    await dispatcher.stop()
    assert not dispatcher._inflight
    assert not dispatcher._dispatch_tasks


def _worker_cost_tracker():
    from echo_agent.cost.budget import CostTracker

    return CostTracker(enabled=True, daily_budget_usd=0.50, pricing_overrides={
        "test-model": {"input_per_1m": 1.0, "output_per_1m": 2.0},
    })


@pytest.mark.asyncio
async def test_worker_records_model_usage_into_shared_cost_budget():
    tracker = _worker_cost_tracker()
    provider = _Provider()
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(
        content="done", usage={"prompt_tokens": 100_000, "completion_tokens": 200_000},
    ))
    executor = WorkerExecutor(provider=provider, default_model="test-model")
    executor._cost_tracker = tracker
    result = await executor.run(
        task_index=0, goal="inspect", tool_defs=[], tool_executor=AsyncMock(),
    )
    assert result.status == "completed"
    assert tracker.spent_usd == pytest.approx(0.50)


@pytest.mark.asyncio
async def test_worker_budget_exhaustion_stops_before_model_call():
    tracker = _worker_cost_tracker()
    await tracker.record("test-model", {"prompt_tokens": 500_000})
    provider = _Provider()
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(content="done"))
    executor = WorkerExecutor(provider=provider, default_model="test-model")
    executor._cost_tracker = tracker
    result = await executor.run(
        task_index=0, goal="inspect", tool_defs=[], tool_executor=AsyncMock(),
    )
    assert result.status == "failed"
    assert "预算" in result.error
    provider.chat_with_retry.assert_not_awaited()


@pytest.mark.asyncio
async def test_worker_checks_budget_again_between_tool_iterations():
    tracker = _worker_cost_tracker()
    provider = _Provider()
    provider.chat_with_retry = AsyncMock(side_effect=[
        LLMResponse(finish_reason="tool_calls", usage={"prompt_tokens": 500_000}, tool_calls=[
            ToolCallRequest(id="read-once", name="read_file", arguments={"path": "report.txt"}),
        ]),
        LLMResponse(content="done"),
    ])
    executor = WorkerExecutor(provider=provider, default_model="test-model")
    executor._cost_tracker = tracker
    result = await executor.run(
        task_index=0, goal="inspect", tool_defs=[],
        tool_executor=AsyncMock(return_value="report"),
    )
    assert result.status == "failed"
    assert "预算" in result.error
    assert provider.chat_with_retry.await_count == 1


def test_runtime_wires_one_cost_tracker_into_delegate_and_spawn(tmp_path):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config

    config = Config()
    config.memory.enabled = False
    config.skills.enabled = False
    config.knowledge.enabled = False
    config.observability.otel_enabled = False
    loop = AgentLoop(MessageBus(), config, _Provider(), tmp_path)
    for name in ("delegate_task", "spawn_task"):
        assert loop.tools.get(name)._executor._cost_tracker is loop.cost_tracker


def _metered_delegation(kind, provider, tracker, router=None, bus=None):
    shared = {
        "provider": provider, "cost_tracker": tracker, "model_router": router,
        "default_model": "test-model", "credentials": None,
    }
    if kind == "delegate":
        return DelegateTool(
            **shared, tool_registry=_worker_registry(), approval_gate=AsyncMock(),
            worker_registry=WorkerRegistry([]),
        )
    return SpawnTool(
        **shared, bus=bus,
        tool_registry=_worker_registry() if kind == "spawn" else None,
        approval_gate=AsyncMock() if kind == "spawn" else None,
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["delegate", "spawn", "spawn_plain"])
async def test_delegation_cost_is_persisted_and_attributed_to_origin(tmp_path, kind):
    from echo_agent.cost.budget import CostTracker
    from echo_agent.storage.sqlite import SQLiteBackend

    storage = SQLiteBackend(tmp_path / "cost.sqlite")
    await storage.initialize()
    try:
        tracker = CostTracker(
            storage=storage, enabled=True, daily_budget_usd=5.0,
            pricing_overrides={"test-model": {"input_per_1m": 1.0, "output_per_1m": 2.0}},
        )
        provider = _Provider()
        provider.chat_with_retry = AsyncMock(return_value=LLMResponse(
            content="done", usage={"prompt_tokens": 100_000, "completion_tokens": 200_000},
        ))
        router = ModelRouter(ModelsConfig(default_model="test-model"))
        router.register_provider("primary", provider)
        bus = AsyncMock()
        tool = _metered_delegation(kind, provider, tracker, router, bus)
        ctx = ToolExecutionContext(channel="telegram", session_key="telegram:test")
        if kind == "delegate":
            result = await tool.execute({"goal": "inspect"}, ctx)
            assert result.success
        else:
            await tool.execute({"task": "inspect"}, ctx)
            await asyncio.gather(*list(tool._tasks.values()))
        rows = await storage.fetch_sql("SELECT * FROM cost_ledger_dim")
        assert len(rows) == 1
        assert rows[0]["channel"] == "telegram"
        assert rows[0]["provider"] == ("default" if kind == "spawn_plain" else "primary")
        assert rows[0]["spent_usd"] == pytest.approx(0.50)
        reloaded = CostTracker(storage=storage)
        await reloaded.load()
        assert reloaded.spent_usd == pytest.approx(0.50)
    finally:
        await storage.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["delegate", "spawn", "spawn_plain"])
async def test_delegation_budget_denial_is_visible_to_caller(kind):
    tracker = _worker_cost_tracker()
    await tracker.record("test-model", {"prompt_tokens": 500_000})
    provider = _Provider()
    provider.chat_with_retry = AsyncMock(return_value=LLMResponse(content="done"))
    bus = AsyncMock()
    tool = _metered_delegation(kind, provider, tracker, bus=bus)
    ctx = ToolExecutionContext(channel="telegram", session_key="telegram:test")
    if kind == "delegate":
        result = await tool.execute({"goal": "inspect"}, ctx)
        assert not result.success
        assert "预算" in result.text
    else:
        await tool.execute({"task": "inspect"}, ctx)
        await asyncio.gather(*list(tool._tasks.values()))
        assert "预算" in bus.publish_outbound.await_args.args[0].text
    provider.chat_with_retry.assert_not_awaited()
