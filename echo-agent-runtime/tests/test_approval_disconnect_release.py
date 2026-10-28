"""断线/中断必须释放挂起的审批,而不是让回合干等超时。

原实现的断线逃逸阀只取消 clarify(gateway/server.py 的 finally 只发
/__clarify_cancel__)。停在审批上的回合没有对应出口:它阻塞在 wait_for_decision,
而且持有会话锁,于是要干等 permissions.approval.wait_timeout_seconds(默认 300 秒),
期间用户的下一条消息排在一个永远不会有人做的决定后面。

Ctrl+C 也一样:中断标记只在推理循环的检查点被轮询,而卡在 wait_for_decision 的回合
永远走不到检查点。
"""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus


def _manager() -> ApprovalManager:
    # default_policy="ask" → request_approval 走 pending 分支。
    return ApprovalManager(default_policy="ask")


def test_pending_approval_records_its_session():
    m = _manager()
    req = m.request_approval("shell", tool_name="shell", params={"cmd": "ls"},
                             user_id="u1", session_key="gateway:cli:c1")
    assert req.status == ApprovalStatus.PENDING
    assert req.session_key == "gateway:cli:c1", "没有会话归属就无法定向释放"


def test_cancel_session_denies_only_that_sessions_approvals():
    m = _manager()
    mine = m.request_approval("shell", tool_name="shell", params={"cmd": "ls"},
                              user_id="u1", session_key="s1")
    others = m.request_approval("shell", tool_name="shell", params={"cmd": "pwd"},
                                user_id="u2", session_key="s2")

    assert m.cancel_session("s1") == 1

    assert m.get(mine.id) is None, "本会话的挂起审批必须被释放"
    assert m.get(others.id) is not None, "不得波及其他会话"


def test_cancel_session_denies_rather_than_approves():
    """释放方向必须是拒绝 —— 这个调用危险到需要人,而人已经不在了。"""
    m = _manager()
    req = m.request_approval("shell", tool_name="shell", params={"cmd": "rm -rf /"},
                             user_id="u1", session_key="s1")
    m.cancel_session("s1")
    decided = m._find_history(req.id)
    assert decided is not None
    assert decided.status == ApprovalStatus.DENIED, "绝不能当成已批准放行"


def test_cancel_session_is_noop_without_session_key():
    m = _manager()
    m.request_approval("shell", tool_name="shell", params={}, user_id="u1", session_key="s1")
    assert m.cancel_session("") == 0, "空会话不得误伤全部挂起审批"
    assert len(m.get_pending()) == 1


@pytest.mark.asyncio
async def test_cancel_session_unblocks_a_waiting_turn():
    """核心场景:阻塞在 wait_for_decision 的回合必须被立即唤醒。

    断言"很快返回"而不是"没超时":原实现下这里要等满 timeout_seconds。
    """
    m = _manager()
    req = m.request_approval("shell", tool_name="shell", params={"cmd": "ls"},
                             user_id="u1", session_key="s1")

    waiter = asyncio.create_task(m.wait_for_decision(req.id, timeout_seconds=300))
    await asyncio.sleep(0)  # 让 waiter 真正进入等待

    m.cancel_session("s1")

    decided = await asyncio.wait_for(waiter, timeout=2.0)
    assert decided is not None
    assert decided.status == ApprovalStatus.DENIED


@pytest.mark.asyncio
async def test_disconnect_command_releases_pending_approval(tmp_path):
    """端到端:网关断线合成的 /__clarify_cancel__ 必须同时释放审批。"""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.events import InboundEvent
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.loader import load_config
    from echo_agent.models.provider import LLMProvider, LLMResponse

    class _Stub(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
            return LLMResponse(content="ok", finish_reason="stop")

        def get_default_model(self):
            return "stub"

    config = load_config(overrides={"workspace": str(tmp_path)})
    # 默认策略是 approve,请求不会进入 pending —— 显式要求 shell 需要人工审批,
    # 才能复现"回合停在审批上"这个场景。
    config.permissions.approval.require_approval = ["shell"]
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_Stub(), workspace=tmp_path)

    session_key = "gateway:cli:c1"
    req = loop.approval.request_approval(
        "shell", tool_name="shell", params={"cmd": "ls"},
        user_id="u1", session_key=session_key,
    )
    assert loop.approval.get(req.id) is not None

    event = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u1", chat_id="c1",
        text=loop._CLARIFY_CANCEL_CMD, session_key_override=session_key,
    )
    await loop._handle_clarify_cancel(event)

    assert loop.approval.get(req.id) is None, "断线后挂起审批必须被释放"
    await loop.stop()


@pytest.mark.asyncio
async def test_interrupt_command_releases_pending_approval(tmp_path):
    """Ctrl+C 合成的中断命令同样要释放审批 —— 否则用户按了也停不下来。"""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.events import InboundEvent
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.loader import load_config
    from echo_agent.models.provider import LLMProvider, LLMResponse

    class _Stub(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
            return LLMResponse(content="ok", finish_reason="stop")

        def get_default_model(self):
            return "stub"

    config = load_config(overrides={"workspace": str(tmp_path)})
    # 默认策略是 approve,请求不会进入 pending —— 显式要求 shell 需要人工审批,
    # 才能复现"回合停在审批上"这个场景。
    config.permissions.approval.require_approval = ["shell"]
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_Stub(), workspace=tmp_path)

    session_key = "gateway:cli:c2"
    req = loop.approval.request_approval(
        "shell", tool_name="shell", params={"cmd": "ls"},
        user_id="u1", session_key=session_key,
    )
    loop.interrupt.request(session_key, "evt-current")

    event = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u1", chat_id="c2",
        text=loop._INTERRUPT_CMD, session_key_override=session_key,
    )
    await loop._handle_interrupt(event)

    assert loop.approval.get(req.id) is None, "中断后挂起审批必须被释放"
    await loop.stop()
