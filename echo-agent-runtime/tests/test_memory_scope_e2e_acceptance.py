"""记忆作用域端到端安全验收：三个 P0 场景 + 迁移不失忆，全走真实 _process_event。"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.models.provider import LLMProvider, LLMResponse


class _StubProvider(LLMProvider):
    def __init__(self):
        super().__init__()
        self._resp = LLMResponse(content="ok", finish_reason="stop")

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return self._resp

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        if self._resp.content and on_delta:
            result = on_delta(self._resp.content)
            if asyncio.iscoroutine(result):
                await result
        return self._resp

    def get_default_model(self):
        return "stub"


def _make_loop(tmp_path: Path, bindings):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config

    config = load_config(overrides={"workspace": str(tmp_path)})
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_StubProvider(), workspace=tmp_path)
    loop.config.memory.cross_channel_owner = True
    loop.config.memory.principal_bindings = list(bindings)
    loop.config.memory.owner_key = "owner"
    return loop


@pytest.mark.asyncio
async def test_write_read_closure_cross_channel(tmp_path):
    # 通道 A(telegram:alice)与 B(slack:U0)都在 bindings→都归一 owner。
    agent = _make_loop(tmp_path, ["telegram:alice", "slack:U0"])
    # seed:owner scope 下的一条 USER 记忆(模拟 A 轮写入的结果)
    agent.memory.add(MemoryEntry(
        type=MemoryType.USER, key="user:city", content="住在上海", source_session="owner",
    ))
    # B 通道走真实 pipeline
    ev_b = InboundEvent.text_message(channel="slack", sender_id="U0", chat_id="U0", text="我住哪")
    await agent._process_event(ev_b, "trace-b")
    # pipeline 把 B 的 memory_scope 冻结为 owner,且 owner 记忆对 B 可见(真链路写→读)
    assert ev_b.memory_scope == "owner"
    entry = next(e for e in agent.memory.list_all(mem_type=MemoryType.USER) if e.key == "user:city")
    assert agent.memory.is_visible_in_session(entry, ev_b.memory_scope) is True


@pytest.mark.asyncio
async def test_group_cron_does_not_read_owner(tmp_path):
    from echo_agent.scheduler.delivery import inbound_event_from_job

    agent = _make_loop(tmp_path, ["telegram:alice"])
    agent.memory.add(MemoryEntry(
        type=MemoryType.USER, key="user:secret", content="owner 私密", source_session="owner",
    ))

    class _Job:
        id = "j1"
        name = "n"
        payload = {"command": "ping", "channel": "telegram", "chat_id": "grp1", "is_group": True}

    ev_cron = inbound_event_from_job(_Job())
    await agent._process_event(ev_cron, "trace-cron")
    # 群聊 cron 不进 owner scope,owner 私密记忆对它不可见
    assert ev_cron.memory_scope != "owner"
    entry = next(e for e in agent.memory.list_all(mem_type=MemoryType.USER) if e.key == "user:secret")
    assert agent.memory.is_visible_in_session(entry, ev_cron.memory_scope) is False


@pytest.mark.asyncio
async def test_real_group_inbound_not_owner(tmp_path):
    # 群聊入站(is_group=True)即使 sender 在 bindings 也不进 owner。
    agent = _make_loop(tmp_path, ["slack:alice"])
    ev = InboundEvent.text_message(
        channel="slack", sender_id="alice", chat_id="C123", text="hi", is_group=True,
    )
    await agent._process_event(ev, "trace-grp")
    assert ev.memory_scope != "owner"


@pytest.mark.asyncio
async def test_migration_recovers_soft_amnesia(tmp_path):
    from echo_agent.cli.migrate_cmd import migrate_source_session

    agent = _make_loop(tmp_path, ["telegram:alice"])
    # 旧格式记忆:source_session 仍是旧通道键
    agent.memory.add(MemoryEntry(
        type=MemoryType.USER, key="user:old", content="旧事实", source_session="telegram:alice",
    ))
    entry = next(e for e in agent.memory.list_all(mem_type=MemoryType.USER) if e.key == "user:old")
    # 迁移前:owner scope 下不可见(软失忆)
    assert agent.memory.is_visible_in_session(entry, "owner") is False
    # 迁移
    migrate_source_session(agent.memory, {"telegram:alice"}, "owner")
    entry2 = next(e for e in agent.memory.list_all(mem_type=MemoryType.USER) if e.key == "user:old")
    # 迁移后:owner scope 下可召回(不失忆)
    assert agent.memory.is_visible_in_session(entry2, "owner") is True
