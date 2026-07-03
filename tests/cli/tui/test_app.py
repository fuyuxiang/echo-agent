import pytest

from echo_agent.cli.tui.bridge import WSBridge
from echo_agent.cli.tui.protocol import CogEvent


class Sink:
    def __init__(self):
        self.events = []
    def on_user_reply_token(self, i, t): self.events.append(("tok", i, t))
    def on_user_reply_final(self, i, t): self.events.append(("fin", i, t))
    def on_cognitive(self, ev): self.events.append(("cog", ev.cog_type, ev.cog_event_id))
    def on_error(self, m): self.events.append(("err", m))


def test_bridge_routes_cognitive_with_dedup():
    s = Sink()
    b = WSBridge(s)
    frame = {"type": "message", "message_kind": "cognitive",
             "text": "召回 1 条", "metadata": {"cog_type": "memory_recalled",
             "cog_event_id": "e1", "_inbound_event_id": "in1", "data": {}}}
    b.dispatch(frame)
    b.dispatch(frame)  # 重复应被去重
    assert s.events == [("cog", "memory_recalled", "e1")]


def test_bridge_routes_error_and_final_text():
    s = Sink()
    b = WSBridge(s)
    b.dispatch({"type": "error", "error": "boom"})
    b.dispatch({"type": "message", "message_kind": "final", "text": "答案",
                "is_final": True, "metadata": {"_inbound_event_id": "in1"}})
    assert ("err", "boom") in s.events
    assert ("fin", "in1", "答案") in s.events


def test_bridge_ignores_control_frames():
    s = Sink()
    b = WSBridge(s)
    for t in ("accepted", "auth_ok", "pong"):
        b.dispatch({"type": t})
    assert s.events == []


def test_bridge_ignores_progress_and_heartbeat_flat_frames():
    # gateway:cli sessions also receive flat progress/tool/heartbeat frames
    # (is_final=False, no _token_stream). They are NOT reply text and must not
    # pop/overwrite an in-flight streaming reply.
    s = Sink()
    b = WSBridge(s)
    meta = {"_inbound_event_id": "in1", "_token_stream": True}
    b.dispatch({"type": "message", "text": "答", "is_final": False, "metadata": meta})
    # progress (empty text) and heartbeat (non-empty text) interleaved
    b.dispatch({"type": "message", "message_kind": "progress", "text": "",
                "is_final": False, "metadata": {"_inbound_event_id": "in1"}})
    b.dispatch({"type": "message", "message_kind": "heartbeat", "text": "还在处理中",
                "is_final": False, "metadata": {"_inbound_event_id": "in1"}})
    b.dispatch({"type": "message", "text": "案", "is_final": False, "metadata": meta})
    # Only the two streaming tokens reached the sink; no final/pop happened.
    assert s.events == [("tok", "in1", "答"), ("tok", "in1", "案")]


@pytest.mark.asyncio
async def test_app_renders_cognitive_and_toggles_memory():
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.blocks import CognitiveBlock
    app = EchoTUI()
    async with app.run_test() as pilot:
        ev = CogEvent("memory_recalled", "e1", "in1",
                      {"items": [{"content": "喜欢深色", "source": "user_stated"}]},
                      "召回 1 条记忆")
        app.on_cognitive(ev)
        await pilot.pause()
        blk = app.query_one(CognitiveBlock)
        assert blk.expanded is False
        await pilot.press("ctrl+r")
        assert blk.expanded is True


@pytest.mark.asyncio
async def test_app_approval_y_sends_command():
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.protocol import CogEvent
    sent = []
    async def fake_send(text): sent.append(text)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        ev = CogEvent("approval_request", "e2", "in1",
                      {"request_id": "req9", "action": "shell", "params": {}, "risk": "EXEC"},
                      "⚠️ 需要确认: shell")
        app.on_cognitive(ev)
        await pilot.pause()
        await pilot.press("y")
        await pilot.pause()
        assert sent == ["/approve req9"]


@pytest.mark.asyncio
async def test_bridge_into_app_end_to_end():
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.bridge import WSBridge
    from echo_agent.cli.tui.blocks import CognitiveBlock
    app = EchoTUI()
    async with app.run_test() as pilot:
        bridge = WSBridge(app)
        bridge.dispatch({"type": "message", "message_kind": "cognitive",
                         "text": "🔧 edit · ok",
                         "metadata": {"cog_type": "tool_call", "cog_event_id": "e1",
                                      "_inbound_event_id": "in1",
                                      "data": {"name": "edit", "status": "ok"}}})
        await pilot.pause()
        assert len(app.query(CognitiveBlock)) == 1


@pytest.mark.asyncio
async def test_app_notify_disconnected_flips_status_bar():
    # A silent ws close (no error frame) must still flip the status bar to the
    # disconnected state — pump() calls notify_disconnected() when its async-for
    # over the socket ends.
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.status_bar import StatusBar
    app = EchoTUI()
    async with app.run_test() as pilot:
        bar = app.query_one(StatusBar)
        assert bar._ok is True
        app.notify_disconnected()
        await pilot.pause()
        assert bar._ok is False
        assert "断开" in str(bar.render())
