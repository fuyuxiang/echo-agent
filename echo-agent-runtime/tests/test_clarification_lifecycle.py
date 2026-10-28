"""End-to-end IM question delivery, task continuation and channel ingress."""
import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.tools.clarify import ClarifyTool
from echo_agent.agent.tools.filesystem import WriteFileTool
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus
from tests.test_inference_stage import _make_stage, _make_ctx, _make_event, _make_session
from tests.test_outbound_final_dedup import _FakeChannel, _manager
from tests.test_task_outcome_writeback import FakeStorage, _running_task


class QuestionProvider(LLMProvider):
    def __init__(self, *, notify=False):
        super().__init__()
        self.calls = 0
        self.notify = notify

    async def chat(self, messages, tools=None, **kwargs):
        self.calls += 1
        if self.calls > 1:
            return LLMResponse(content="已按选择完成", finish_reason="stop")
        calls = [ToolCallRequest(id="q", name="clarify", arguments={
            "question": "选哪一项？", "options": ["甲", "乙"],
        })]
        if self.notify:
            calls.insert(0, ToolCallRequest(id="notice", name="message", arguments={
                "channel": "weixin", "chat_id": "room", "text": "先给你一个通知",
            }))
        return LLMResponse(tool_calls=calls, finish_reason="tool_calls")

    async def chat_stream(self, messages, tools=None, **kwargs):
        return await self.chat(messages, tools, **kwargs)

    def get_default_model(self):
        return "stub"


def make_loop(tmp_path, bus, provider=None, **kwargs):
    config = load_config(overrides={"workspace": str(tmp_path), "memory": {"enabled": False},
                                    "observability": {"otel_enabled": False}})
    loop = AgentLoop(bus=bus, config=config, provider=provider or QuestionProvider(), workspace=tmp_path, **kwargs)
    loop._running = True
    return loop


@pytest.mark.asyncio
async def test_notification_cannot_suppress_the_actual_question(tmp_path):
    bus, channel = MessageBus(), _FakeChannel()
    _manager(bus, channel)
    loop = make_loop(tmp_path, bus, QuestionProvider(notify=True))
    loop.approval_gate.check = AsyncMock(return_value=MagicMock(denial=None, approved_actions=frozenset()))
    await loop._on_inbound(InboundEvent.text_message("weixin", "u", "room", "通知后提问"))
    assert len(channel.sent) == 2
    assert "选哪一项？" in channel.sent[1]
    assert loop.clarify.peek_im_pending("weixin:room", 300, "u") is not None


@pytest.mark.asyncio
async def test_tools_after_question_wait_for_a_real_answer(tmp_path):
    clarify, tools = ClarifyManager(), ToolRegistry()
    tools.register(ClarifyTool(clarify))
    tools.register(WriteFileTool(str(tmp_path), restrict=True))
    provider = AsyncMock()
    provider.chat_stream_with_retry.return_value = LLMResponse(tool_calls=[
        ToolCallRequest(id="q", name="clarify", arguments={"question": "保存还是取消？", "options": ["保存", "取消"]}),
        ToolCallRequest(id="write", name="write_file", arguments={"path": "wait.txt", "content": "must wait"}),
    ], finish_reason="tool_calls")
    stage, _ = _make_stage(provider=provider, tools=tools)
    stage._clarify = clarify
    event = _make_event()
    event.channel = "dingtalk"
    session = _make_session()
    session.metadata = {}
    result = await stage.run(_make_ctx(event=event, session=session, tool_defs=tools.get_definitions("dingtalk")))
    assert result.termination_reason == "awaiting_clarification"
    assert not (tmp_path / "wait.txt").exists()
    deferred = [call for call in session.add_message.call_args_list if call.kwargs.get("tool_call_id") == "write"]
    assert len(deferred) == 1 and "did not run" in deferred[0].args[1]


@pytest.mark.asyncio
async def test_waiting_task_survives_restart_and_answer_completes_the_same_task(tmp_path):
    bus = MessageBus()
    bus.subscribe_outbound("dingtalk", AsyncMock(return_value=SendResult(success=True)))
    tasks = TaskManager(FakeStorage())
    task = await _running_task(tasks)
    loop = make_loop(tmp_path, bus, task_manager=tasks)
    await loop._on_inbound(InboundEvent.text_message("dingtalk", "u", "room", "执行任务", metadata={"task_id": task.id}))
    waiting = await tasks.get(task.id)
    assert waiting.status == TaskStatus.BLOCKED and not waiting.error
    assert "选哪一项" in waiting.blocked_reason
    restarted = make_loop(tmp_path, bus, task_manager=tasks)
    restarted.provider.calls = 1
    answer = InboundEvent.text_message("dingtalk", "u", "room", "b")
    await restarted._on_inbound(answer)
    assert answer.clarification_answer and answer.metadata["task_id"] == task.id
    assert "选哪一项" in answer.reply_to_text
    assert (await tasks.get(task.id)).status == TaskStatus.SUCCESS


@pytest.mark.asyncio
async def test_failed_delivery_cannot_bind_a_concurrent_unrelated_message(tmp_path):
    bus, entered, release = MessageBus(), asyncio.Event(), asyncio.Event()
    async def receive(event):
        if event.metadata.get("_inbound_event_id") == "q":
            entered.set()
            await release.wait()
            return SendResult(success=False)
        return SendResult(success=True)
    bus.subscribe_outbound("dingtalk", receive)
    loop = make_loop(tmp_path, bus)
    first = asyncio.create_task(loop._on_inbound(InboundEvent.text_message("dingtalk", "u", "room", "开始", event_id="q")))
    await asyncio.wait_for(entered.wait(), 5)
    unrelated = InboundEvent.text_message("dingtalk", "u", "room", "今天几点开会？")
    second = asyncio.create_task(loop._on_inbound(unrelated))
    await asyncio.sleep(0)
    assert not unrelated.reply_to_text
    assert loop.clarify.peek_im_pending("dingtalk:room", 300, "u") is None
    release.set()
    await asyncio.wait_for(asyncio.gather(first, second), 5)
    assert not unrelated.reply_to_text


@pytest.mark.asyncio
async def test_board_question_can_be_answered_once_and_dispatched_in_the_same_session(tmp_path):
    from echo_agent.tasks.dispatcher import TaskDispatcher
    from echo_agent.gateway.api.tasks import TasksAPI
    from echo_agent.bus.events import EventType
    from types import SimpleNamespace
    bus, tasks = MessageBus(), TaskManager(FakeStorage())
    task = await _running_task(tasks)
    key = f"task:{task.id}"
    await tasks.set_running_context(task.id, key, "start")
    loop = make_loop(tmp_path, bus, task_manager=tasks)
    source = InboundEvent.text_message("task", "dispatcher", task.id, "执行任务",
                                       event_type=EventType.SYSTEM, unattended=True,
                                       session_key_override=key, metadata={"task_id": task.id})
    await loop._on_inbound(source)
    waiting = await tasks.get(task.id)
    assert waiting.status == TaskStatus.BLOCKED
    question = waiting.metadata["_clarification"]
    api = TasksAPI(SimpleNamespace(_agent_loop=loop, _require_admin_token=lambda *a, **k: None))
    req = MagicMock(match_info={"id": task.id})
    req.json = AsyncMock(return_value={"question_id": question["id"], "answer": "b"})
    assert (await api.answer_clarification(req)).status == 200
    assert (await api.answer_clarification(req)).status == 409
    dispatcher = TaskDispatcher(bus, tasks)
    tasks.add_terminal_listener(dispatcher._on_task_terminal)
    events = []
    async def publish(event):
        events.append(event)
        await loop._on_inbound(event)
        return True
    bus.publish_inbound = publish
    await asyncio.wait_for(dispatcher._dispatch(await tasks.get(task.id)), 5)
    assert events[0].session_key == key and events[0].clarification_answer
    assert events[0].unattended and not events[0].cron_authorized and not events[0].channel_verified
    assert (await tasks.get(task.id)).status == TaskStatus.SUCCESS


@pytest.mark.asyncio
async def test_cancelled_task_answer_never_runs_the_model(tmp_path):
    bus, tasks = MessageBus(), TaskManager(FakeStorage())
    bus.subscribe_outbound("dingtalk", AsyncMock(return_value=SendResult(success=True)))
    task = await _running_task(tasks)
    loop = make_loop(tmp_path, bus, task_manager=tasks)
    await loop._on_inbound(InboundEvent.text_message("dingtalk", "u", "room", "执行", metadata={"task_id": task.id}))
    await tasks.cancel(task.id)
    calls = loop.provider.calls
    await loop._on_inbound(InboundEvent.text_message("dingtalk", "u", "room", "b"))
    assert loop.provider.calls == calls
    assert (await tasks.get(task.id)).status == TaskStatus.CANCELLED


@pytest.mark.asyncio
async def test_sqlite_admits_only_one_board_answer_and_rejects_cancelled_questions(tmp_path):
    from echo_agent.storage.sqlite import SQLiteBackend
    storage = SQLiteBackend(tmp_path / "task-answer.db")
    await storage.initialize()
    try:
        manager = TaskManager(storage)
        task = await _running_task(manager)
        await manager.update(task.id, metadata={"_awaiting_clarification": True,
                                                "_clarification": {"id": "q", "channel": "task"}})
        await manager.transition(task.id, TaskStatus.BLOCKED)
        with pytest.raises(ValueError):
            await manager.answer_clarification(task.id, "old-q", "b")
        results = await asyncio.gather(manager.answer_clarification(task.id, "q", "a"),
                                       manager.answer_clarification(task.id, "q", "b"), return_exceptions=True)
        assert sum(isinstance(result, ValueError) for result in results) == 1
        assert (await manager.get(task.id)).status == TaskStatus.QUEUED
        await manager.cancel(task.id)
        with pytest.raises(ValueError):
            await manager.answer_clarification(task.id, "q", "b")
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_runtime_hook_failure_keeps_the_question_for_retry(tmp_path):
    from echo_agent.plugins.runtime import RuntimeExtension
    bus = MessageBus()
    bus.subscribe_outbound("dingtalk", AsyncMock(return_value=SendResult(success=True)))
    loop = make_loop(tmp_path, bus)
    await loop._on_inbound(InboundEvent.text_message("dingtalk", "u", "room", "开始"))
    class FailedHook(RuntimeExtension):
        async def before_turn(self, manager):
            raise RuntimeError("temporary failure")
    loop._runtime_extension = FailedHook()
    first = InboundEvent.text_message("dingtalk", "u", "room", "b")
    await loop._on_inbound(first)
    assert not first.clarification_answer
    assert (await loop.sessions.get("dingtalk:room")).metadata["_im_clarification"]
    loop._runtime_extension = None
    second = InboundEvent.text_message("dingtalk", "u", "room", "b")
    await loop._on_inbound(second)
    assert second.clarification_answer


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["telegram", "feishu"])
async def test_group_answer_bypasses_mentions_only_for_the_question_owner(tmp_path, name):
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.channels.feishu import FeishuChannel
    from echo_agent.config.schema import TelegramChannelConfig, FeishuChannelConfig
    bus = MessageBus()
    loop = make_loop(tmp_path, bus)
    scope = loop.config.session.group_session_scope
    source = InboundEvent(channel=name, sender_id="u", chat_id="group", is_group=True)
    loop.clarify.register_im_pending(source.scoped_session_key(scope), "选哪项？", ["甲", "乙"], user_id="u")
    if name == "telegram":
        channel = TelegramChannel(TelegramChannelConfig(token="token", group_policy="mention"), bus)
        channel._handle_message = AsyncMock()
        async def send(sender):
            await channel._process_update({"message": {"chat": {"id": "group", "type": "supergroup"},
                                                        "from": {"id": sender}, "text": "b", "message_id": 2}})
    else:
        channel = FeishuChannel(FeishuChannelConfig(app_id="app", app_secret="secret", group_policy="mention"), bus)
        channel._handle_message = AsyncMock()
        async def send(sender):
            await channel._on_message({"sender": {"sender_id": {"open_id": sender}, "sender_type": "user"},
                                       "message": {"chat_id": "group", "chat_type": "group", "message_type": "text",
                                                   "message_id": "answer", "content": '{"text":"b"}'}})
    await send("other")
    assert channel._handle_message.await_count == 0
    await send("u")
    assert channel._handle_message.await_count == 1
