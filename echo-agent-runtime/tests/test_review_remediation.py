"""Regression evidence for the uncommitted-change review and its recovery paths."""

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
import pytest_asyncio

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import InboundEvent
from echo_agent.config.loader import load_config
from echo_agent.config.schema import Config
from echo_agent.evaluation.dataset import EvalCase
from echo_agent.evaluation.runtime import IsolatedEvalRunner
from echo_agent.gateway.api.channels import ChannelsAPI
from echo_agent.gateway.api.config import ConfigAPI
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.scheduler.service import JobStatus, ScheduledJob, Scheduler, TriggerKind
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus


class Request:
    def __init__(self, changes=None, **match_info):
        self.changes, self.match_info = changes, match_info

    async def json(self):
        return {"changes": self.changes}


def server_for(config, path):
    return SimpleNamespace(
        _agent_loop=SimpleNamespace(config=config), _config_path=path,
        _require_admin_token=lambda *a, **k: None,
        dashboard_ws=SimpleNamespace(broadcast=AsyncMock()),
    )


@pytest.mark.asyncio
async def test_channel_activation_preserves_environment_credentials_and_allowlist(tmp_path, monkeypatch):
    path = tmp_path / "config.yaml"
    path.write_text('channels:\n  telegram:\n    enabled: true\n    token: ""\n    allowFrom: []\n')
    monkeypatch.setenv("ECHO_AGENT_CHANNELS__TELEGRAM__TOKEN", "test-env-secret")
    monkeypatch.setenv("ECHO_AGENT_CHANNELS__TELEGRAM__ALLOW_FROM", '["owner"]')
    config = load_config(path)
    server = server_for(config, path)
    manager = SimpleNamespace(config=config.channels, get_channel=lambda name: None,
                              restart_channel=AsyncMock(return_value=SimpleNamespace(is_running=True)))
    server.channel_manager = manager
    assert (await ConfigAPI(server).update_config(Request({"ui.locale": "zh"}))).status == 200
    assert "test-env-secret" not in path.read_text()
    assert (await ChannelsAPI(server).lifecycle(Request(name="telegram", action="restart"))).status == 200
    assert manager.config.telegram.token == "test-env-secret"
    assert manager.config.telegram.allow_from == ["owner"]
    body = json.loads((await ConfigAPI(server).get_config(Request())).body)
    assert "channels.telegram.allow_from" in body["_meta"]["managed_paths"]
    assert "channels.telegram.token" not in body["_meta"]["pending_paths"]
    assert "test-env-secret" not in json.dumps(body)


@pytest.mark.asyncio
async def test_startup_managed_setting_is_not_saved_as_an_ineffective_edit(tmp_path, monkeypatch):
    path = tmp_path / "config.yaml"
    path.write_text("memory:\n  enabled: false\n")
    monkeypatch.setenv("ECHO_AGENT_MEMORY__ENABLED", "true")
    runtime = load_config(path, overrides={"ui": {"locale": "en"}})
    server = server_for(runtime, path)
    before = path.read_bytes()
    for changes in ({"memory.enabled": False}, {"ui.locale": "zh"}):
        response = await ConfigAPI(server).update_config(Request(changes))
        assert response.status == 409
        assert json.loads(response.body)["code"] == "config_managed"
        assert path.read_bytes() == before
    body = json.loads((await ConfigAPI(server).get_config(Request())).body)
    assert body["memory"]["enabled"] is True
    assert body["ui"]["locale"] == "en"
    assert body["_meta"]["restart_required"] is False


def test_removed_yaml_setting_uses_defaults_instead_of_old_runtime_value(tmp_path):
    path = tmp_path / "config.yaml"
    path.write_text("memory:\n  enabled: false\n")
    runtime = load_config(path)
    path.write_text("{}\n")
    saved, _ = ConfigAPI(server_for(runtime, path))._saved_config(runtime)
    assert saved.memory.enabled is True
    assert runtime.memory.enabled is False


@pytest.mark.asyncio
@pytest.mark.parametrize("port", [0, 59081])
async def test_gateway_listen_arguments_remain_authoritative_after_config_reload(tmp_path, monkeypatch, port):
    from echo_agent import app

    path = tmp_path / "config.yaml"
    path.write_text("gateway:\n  host: 0.0.0.0\n  port: 58123\n")
    monkeypatch.setattr(app, "_gateway_port_in_use", lambda *args: None)
    captured = []

    class StopBeforeRuntime(Exception):
        pass

    async def bootstrap(**kwargs):
        captured.append(load_config(kwargs["config_path"], overrides=kwargs["overrides"]))
        raise StopBeforeRuntime

    monkeypatch.setattr(app, "bootstrap", bootstrap)
    with pytest.raises(StopBeforeRuntime):
        await app.run_gateway(config_path=str(path), host="127.0.0.1", port=port)
    runtime = captured[0]
    assert runtime.gateway.host == "127.0.0.1" and runtime.gateway.port == port
    api = ConfigAPI(server_for(runtime, path))
    saved, _ = api._saved_config(runtime)
    assert saved.gateway == runtime.gateway
    response = json.loads((await api.get_config(Request())).body)
    assert response["_meta"]["pending_paths"] == []


@pytest.mark.asyncio
async def test_empty_section_cannot_erase_credentials_but_empty_mapping_can_clear_overrides(tmp_path):
    path = tmp_path / "config.yaml"
    path.write_text("channels:\n  telegram:\n    token: keep-secret\ncost:\n  pricing_overrides:\n    test: {input: 1}\n")
    api = ConfigAPI(server_for(load_config(path), path))
    assert (await api.update_config(Request({"channels.telegram": {}}))).status == 400
    assert "keep-secret" in path.read_text()
    assert (await api.update_config(Request({"cost.pricing_overrides": {}}))).status == 200
    saved, _ = api._saved_config(api._get_config())
    assert saved.cost.pricing_overrides == {}


@pytest.mark.asyncio
@pytest.mark.parametrize("broken", ["channels: [invalid", "memory: {enabled: [invalid]}"])
async def test_stop_channel_does_not_need_valid_saved_config(tmp_path, broken):
    path = tmp_path / "config.yaml"
    path.write_text(broken)
    config = Config()
    config.channels.telegram.token = "running-secret"
    server = server_for(config, path)
    stop = AsyncMock()
    server.channel_manager = SimpleNamespace(config=config.channels, stop_channel=stop, get_channel=lambda name: None)
    response = await ChannelsAPI(server).lifecycle(Request(name="telegram", action="stop"))
    assert response.status == 200
    stop.assert_awaited_once_with("telegram")
    assert config.channels.telegram.token == "running-secret"
    assert path.read_text() == broken


@pytest_asyncio.fixture
async def state(tmp_path):
    storage = SQLiteBackend(tmp_path / "state.db")
    await storage.initialize()
    yield storage, TaskManager(storage)
    await storage.close()


async def running_task(manager, *, attempt="old", event="old-event"):
    task = await manager.create("work")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.set_running_context(task.id, f"task:{task.id}", event, attempt_id=attempt)
    return await manager.get(task.id)


@pytest.mark.asyncio
async def test_old_clarification_cannot_block_new_attempt(state):
    _, manager = state
    task = await running_task(manager)
    await manager.mark_terminal(task.id, TaskStatus.FAILED, expected_attempt_id="old")
    await manager.retry(task.id)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.set_running_context(task.id, task.session_id, "new-event", attempt_id="new")
    loop = AgentLoop.__new__(AgentLoop)
    loop._task_manager = manager
    event = InboundEvent(channel="task", chat_id=task.id, event_id="old-event",
                         metadata={"task_id": task.id, "task_attempt_id": "old"})
    assert not await loop._wait_task_for_clarification(event, "old question")
    final = await manager.get(task.id)
    assert final.status == TaskStatus.RUNNING and final.attempt_id == "new"
    assert not final.blocked_reason and not final.metadata.get("_awaiting_clarification")


@pytest.mark.asyncio
async def test_clarification_cas_retry_rechecks_attempt_identity(state, monkeypatch):
    storage, manager = state
    task = await running_task(manager)
    entered, release = asyncio.Event(), asyncio.Event()
    persist = storage.cas_store_task

    async def delayed(task_id, data, version):
        if asyncio.current_task().get_name() == "old-clarification" and not entered.is_set():
            entered.set()
            await release.wait()
        return await persist(task_id, data, version)

    monkeypatch.setattr(storage, "cas_store_task", delayed)
    writer = asyncio.create_task(manager.block_for_clarification(task.id, "old", attempt_id="old"),
                                 name="old-clarification")
    await asyncio.wait_for(entered.wait(), 2)
    await manager.mark_terminal(task.id, TaskStatus.FAILED, expected_attempt_id="old")
    await manager.retry(task.id)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.set_running_context(task.id, task.session_id, "new-event", attempt_id="new")
    release.set()
    assert await asyncio.wait_for(writer, 2) is None
    final = await manager.get(task.id)
    assert final.status == TaskStatus.RUNNING and final.attempt_id == "new"
    assert not final.metadata.get("_awaiting_clarification")


@pytest.mark.asyncio
async def test_stale_answer_cannot_resume_a_new_attempts_question(state):
    _, manager = state
    task = await running_task(manager, attempt="new", event="new-event")
    await manager.block_for_clarification(task.id, "new question", attempt_id="new")
    assert await manager.resume_after_clarification(task.id, task.session_id, "answer", attempt_id="old") is None
    resumed = await manager.resume_after_clarification(task.id, task.session_id, "answer", attempt_id="new")
    assert resumed.status == TaskStatus.RUNNING and resumed.attempt_id != "new"
    assert resumed.metadata["_interrupt_event_id"] == "answer"
    assert "_awaiting_clarification" not in resumed.metadata


@pytest.mark.asyncio
@pytest.mark.parametrize("action", ["cancel", "update", "fail", "complete"])
async def test_late_task_tool_cannot_mutate_its_replacement_attempt(state, action):
    from echo_agent.agent.tools.task import TaskTool
    from echo_agent.tools import ToolExecutionContext

    _, manager = state
    task = await running_task(manager, attempt="new", event="new-event")
    result = await TaskTool(manager).execute(
        {"action": action, "task_id": task.id, "title": "old title"},
        ToolExecutionContext(session_key=task.session_id, inbound_event_id="old-event"),
    )
    assert not result.success
    final = await manager.get(task.id)
    assert final.status == TaskStatus.RUNNING and final.title == "work"


@pytest.mark.asyncio
@pytest.mark.parametrize("trigger", [TriggerKind.INTERVAL, TriggerKind.ONCE])
async def test_failed_pre_dispatch_persistence_releases_capacity_and_allows_recovery(tmp_path, monkeypatch, trigger):
    handler = AsyncMock(return_value="completed")
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=handler, max_concurrent=1)
    first = scheduler.add_job(ScheduledJob(id="first", trigger=trigger, interval_ms=60000))
    second = scheduler.add_job(ScheduledJob(id="second", interval_ms=60000))
    with monkeypatch.context() as patch:
        patch.setattr(scheduler, "_write_locked", lambda payload: (_ for _ in ()).throw(OSError("disk unavailable")))
        assert not await scheduler.trigger_job(first.id)
    handler.assert_not_awaited()
    assert not first.active_run_id and first.last_status == "error"
    assert first.status == JobStatus.ACTIVE
    assert await scheduler.trigger_job(second.id)
    assert await scheduler.trigger_job(first.id)
    assert [row["status"] for row in first.run_history] == ["error", "completed"]


@pytest.mark.asyncio
async def test_cancel_during_pre_dispatch_save_releases_unpublished_run(tmp_path, monkeypatch):
    handler, entered = AsyncMock(return_value="queued"), asyncio.Event()
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=handler)
    job = scheduler.add_job(ScheduledJob(id="cancel", interval_ms=60000))
    save, calls = scheduler._save_async, 0

    async def first_save_waits():
        nonlocal calls
        calls += 1
        if calls == 1:
            entered.set()
            await asyncio.Event().wait()
        await save()

    monkeypatch.setattr(scheduler, "_save_async", first_save_waits)
    task = asyncio.create_task(scheduler.trigger_job(job.id))
    await asyncio.wait_for(entered.wait(), 2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, 2)
    assert not job.active_run_id and job.last_status == "interrupted"
    assert "no work was submitted" in job.last_error
    handler.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_after_one_shot_handler_started_does_not_replay_it(tmp_path):
    entered = asyncio.Event()

    async def handler(job):
        entered.set()
        await asyncio.Event().wait()

    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=handler)
    job = scheduler.add_job(ScheduledJob(id="once", trigger=TriggerKind.ONCE))
    task = asyncio.create_task(scheduler.trigger_job(job.id))
    await asyncio.wait_for(entered.wait(), 2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, 2)
    assert job.status == JobStatus.COMPLETED and job.last_status == "interrupted"
    assert not await scheduler.trigger_job(job.id)


class RoutedProvider(LLMProvider):
    def __init__(self, name):
        super().__init__()
        self.name, self.models = name, []

    def get_default_model(self):
        return self.name

    async def chat(self, messages, tools=None, model=None, **kwargs):
        self.models.append(model)
        return LLMResponse(content="ok" if model == self.name else "wrong provider",
                           finish_reason="stop" if model == self.name else "error")


@pytest.mark.asyncio
async def test_isolated_evaluation_routes_default_model_to_its_registered_provider(tmp_path):
    config = Config.model_validate({"models": {
        "default_model": "model-b", "providers": [
            {"name": "a", "models": ["model-a"]}, {"name": "b", "models": ["model-b"]},
        ],
    }, "memory": {"enabled": False}, "knowledge": {"enabled": False}, "planning": {"enabled": False}})
    first, second = RoutedProvider("model-a"), RoutedProvider("model-b")
    runner = IsolatedEvalRunner(config=config, provider=first, providers={"a": first, "b": second},
                                skill_store=SkillStore(user_dir=tmp_path / "skills"))
    result = await runner.run_case(EvalCase(id="route", input="hello", expected_contains=["ok"]))
    assert result.passed, result.error
    assert first.models == [] and second.models == ["model-b"]


@pytest.mark.asyncio
async def test_failed_summary_and_cooldown_create_no_archives(state):
    from echo_agent.agent.compression.compressor import ConversationCompressor
    from echo_agent.config.schema import CompressionConfig

    storage, _ = state
    provider = SimpleNamespace(chat_with_retry=AsyncMock(return_value=LLMResponse(content="failed", finish_reason="error")))
    compressor = ConversationCompressor(CompressionConfig(tool_pruning_enabled=False, head_protect_count=2),
                                       1000, provider, "test", storage=storage)
    messages = [{"role": "user" if i % 2 == 0 else "assistant", "content": "x" * 400} for i in range(30)]
    for _ in range(2):
        result = await compressor.compress(messages)
        assert not result.was_compressed and result.messages == messages
    assert provider.chat_with_retry.await_count == 1
    rows = await storage.fetch_sql("SELECT COUNT(*) AS count FROM message_archive")
    assert rows[0]["count"] == 0
    compressor._stats.last_summary_failure_at = None
    provider.chat_with_retry.return_value = LLMResponse(content="usable summary")
    assert (await compressor.compress(messages)).was_compressed
    rows = await storage.fetch_sql("SELECT COUNT(*) AS count FROM message_archive")
    assert rows[0]["count"] == 1
