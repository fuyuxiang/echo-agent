"""草稿撤回必须真正清掉 TUI 上的文本,而不是只重置服务端缓冲。

原实现的 discard() 只清 publisher 内部状态,不发任何帧。TUI 侧按 inbound_id 复用
同一个 AgentReply,append_token 是累加,于是工具执行后的新流式 token 拼在旧草稿后
面,界面上呈现为:

    旧草稿 → 旧草稿最终答案流 → (final 帧才变成) 最终答案流

也就是最终答案流式期间一直显示拼接错误的文本。这里覆盖三层:publisher 发出 reset、
bridge 路由 reset、AgentReply 真的被清空。
"""

from __future__ import annotations

import pytest

from echo_agent.agent.streaming import TokenStreamPublisher
from echo_agent.bus.events import InboundEvent
from echo_agent.cli.tui.bridge import WSBridge


class _RecordingBus:
    def __init__(self) -> None:
        self.published: list = []

    async def publish_outbound(self, event):
        self.published.append(event)
        from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
        return DeliveryResult(DeliveryStage.DELIVERED, event.channel)


class _Sink:
    def __init__(self) -> None:
        self.calls: list = []

    def on_user_reply_token(self, i, t): self.calls.append(("tok", i, t))
    def on_user_reply_final(self, i, t): self.calls.append(("fin", i, t))
    def on_user_reply_reset(self, i): self.calls.append(("reset", i))
    def on_tool_delivery(self, i, d, t): self.calls.append(("delivery", i, d, t))
    def on_cognitive(self, ev): self.calls.append(("cog",))
    def on_error(self, m): self.calls.append(("err", m))


def _publisher(bus) -> TokenStreamPublisher:
    event = InboundEvent.text_message(channel="gateway:cli", sender_id="u", chat_id="c", text="q")
    return TokenStreamPublisher(
        bus, event, enabled=True, flush_chars=1, flush_interval_ms=50,
        paragraph_mode=False,
    )


# ── publisher:撤回必须发出显式 reset 帧 ────────────────────────────────────


@pytest.mark.asyncio
async def test_discard_publishes_reset_frame():
    bus = _RecordingBus()
    pub = _publisher(bus)
    await pub.on_delta("让我查一下")  # flush_chars=1 → 立即发出
    assert bus.published, "草稿应已发出"

    await pub.discard()

    reset_frames = [e for e in bus.published if e.metadata.get("_stream_reset")]
    assert len(reset_frames) == 1, "撤回必须发出恰好一帧 reset"
    frame = reset_frames[0]
    assert frame.is_final is False, "reset 属于流式阶段,不是终帧"
    assert frame.message_kind == "streaming"
    assert frame.metadata.get("_token_stream") is True
    assert frame.metadata.get("_inbound_event_id"), "reset 必须带 turn 关联 id"


@pytest.mark.asyncio
async def test_discard_without_published_draft_sends_nothing():
    """草稿还没出去就撤回时不发 reset —— 否则通道会凭空开一个空回复块。"""
    bus = _RecordingBus()
    pub = _publisher(bus)
    await pub.discard()
    assert bus.published == []


@pytest.mark.asyncio
async def test_stream_after_discard_is_not_concatenated():
    """撤回后重新流式,最终答案不得带上旧草稿的任何片段。"""
    bus = _RecordingBus()
    pub = _publisher(bus)
    await pub.on_delta("让我查一下")
    await pub.discard()
    await pub.on_delta("答案是 42")
    await pub.finalize("答案是 42")

    final = [e for e in bus.published if e.is_final][-1]
    assert final.text == "答案是 42"
    assert "让我查一下" not in final.text


# ── bridge:reset 帧必须路由到 sink 的清空动作 ──────────────────────────────


def test_bridge_routes_reset_frame_to_sink():
    sink = _Sink()
    bridge = WSBridge(sink)
    meta = {"_inbound_event_id": "in1", "_token_stream": True}
    bridge.dispatch({"type": "message", "text": "让我查一下", "is_final": False, "metadata": meta})
    bridge.dispatch({
        "type": "message", "text": "", "is_final": False,
        "metadata": {**meta, "_stream_reset": True},
    })
    bridge.dispatch({"type": "message", "text": "答案", "is_final": False, "metadata": meta})

    assert sink.calls == [
        ("tok", "in1", "让我查一下"),
        ("reset", "in1"),
        ("tok", "in1", "答案"),
    ], f"reset 未被正确路由: {sink.calls}"


def test_bridge_reset_is_not_treated_as_empty_token():
    """回归:reset 帧不能当成普通空 token 放过去。

    放过去的话 TUI 什么都不会清,append_token('') 是无操作,拼接照旧发生。
    """
    sink = _Sink()
    bridge = WSBridge(sink)
    bridge.dispatch({
        "type": "message", "text": "", "is_final": False,
        "metadata": {"_inbound_event_id": "in1", "_token_stream": True, "_stream_reset": True},
    })
    assert sink.calls == [("reset", "in1")]


# ── AgentReply:清空后界面上不能残留旧文本 ─────────────────────────────────


def test_agent_reply_clear_stream_drops_text():
    from echo_agent.cli.tui.blocks import AgentReply

    r = AgentReply.__new__(AgentReply)
    r._buf = ""
    rendered: list[str] = []
    r.update = lambda content, **kw: rendered.append(content)

    r.append_token("让我查一下")
    assert "让我查一下" in rendered[-1]

    r.clear_stream()
    assert r.text == "", "清空后 /copy 也不该还拿到草稿"
    assert "让我查一下" not in rendered[-1], "清空后界面仍显示旧草稿"

    r.append_token("答案")
    assert "让我查一下" not in rendered[-1], "新 token 不得拼在旧草稿之后"
    assert "答案" in rendered[-1]


@pytest.mark.asyncio
async def test_app_reset_clears_the_reply_widget():
    """端到端:reset 帧经 bridge 落到 app,对应回复块必须被清空。"""
    from echo_agent.cli.tui.app import EchoTUI

    app = EchoTUI(session_key="s")
    async with app.run_test():
        meta = {"_inbound_event_id": "in1", "_token_stream": True}
        bridge = WSBridge(app)
        bridge.dispatch({"type": "message", "text": "让我查一下", "is_final": False, "metadata": meta})
        reply = app._replies["in1"]
        assert "让我查一下" in reply.text

        bridge.dispatch({
            "type": "message", "text": "", "is_final": False,
            "metadata": {**meta, "_stream_reset": True},
        })
        assert reply.text == "", "撤回后回复块必须为空"

        bridge.dispatch({"type": "message", "text": "答案是 42", "is_final": False, "metadata": meta})
        assert reply.text == "答案是 42", f"新流式内容被污染: {reply.text!r}"
