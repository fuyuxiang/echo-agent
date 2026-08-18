import pytest

from echo_agent.cli.tui.bridge import WSBridge
from echo_agent.cli.tui.protocol import CogEvent


class Sink:
    def __init__(self):
        self.events = []
    def on_user_reply_token(self, i, t): self.events.append(("tok", i, t))
    def on_user_reply_final(self, i, t): self.events.append(("fin", i, t))
    def on_user_reply_reset(self, i): self.events.append(("reset", i))
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
    from echo_agent.cli.tui.prompt_input import PromptInput
    sent = []
    async def fake_send(text): sent.append(text)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        ev = CogEvent("approval_request", "e2", "in1",
                      {"request_id": "req9", "action": "shell", "params": {}, "risk": "EXEC"},
                      "⚠️ 需要确认: shell")
        app.on_cognitive(ev)
        await pilot.pause()
        assert app.query_one(PromptInput).disabled is True
        await pilot.press("y")
        await pilot.pause()
        assert sent == ["/approve req9"]
        # 决定后输入框重新启用
        assert app.query_one(PromptInput).disabled is False


@pytest.mark.asyncio
async def test_app_approval_mouse_click_cannot_break_yna():
    """回归:审批待定时鼠标点输入框,过去会让 y/n/a 被输入框吞掉而失灵。

    输入框禁用后点击无法聚焦它,y 仍走 App 级 binding 正常批准。"""
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.protocol import CogEvent
    from echo_agent.cli.tui.prompt_input import PromptInput
    sent = []
    async def fake_send(text): sent.append(text)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        ev = CogEvent("approval_request", "e2", "in1",
                      {"request_id": "req9", "action": "shell", "params": {}, "risk": "EXEC"},
                      "⚠️ 需要确认: shell")
        app.on_cognitive(ev)
        await pilot.pause()
        pi = app.query_one(PromptInput)
        await pilot.click(PromptInput)      # 禁用态下点击不应聚焦
        await pilot.pause()
        assert pi.disabled is True
        assert app.focused is None
        await pilot.press("y")
        await pilot.pause()
        assert sent == ["/approve req9"]


@pytest.mark.asyncio
async def test_app_cost_update_only_updates_status_bar():
    # cost_update must refresh the status bar cost but NOT append a CognitiveBlock
    # to the transcript, otherwise tool-heavy turns spam 💰 blocks.
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.blocks import CognitiveBlock
    from echo_agent.cli.tui.status_bar import StatusBar
    app = EchoTUI()
    async with app.run_test() as pilot:
        before = len(app.query(CognitiveBlock))
        ev = CogEvent("cost_update", "e3", "in1", {"total_cost": 0.042}, "累计 $0.042")
        app.on_cognitive(ev)
        await pilot.pause()
        assert len(app.query(CognitiveBlock)) == before
        assert app.query_one(StatusBar)._cost == 0.042


@pytest.mark.asyncio
async def test_bridge_into_app_end_to_end():
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.bridge import WSBridge
    from echo_agent.cli.tui.blocks import ToolCallBlock
    app = EchoTUI()
    async with app.run_test() as pilot:
        bridge = WSBridge(app)
        # tool_call frames route to a dedicated ToolCallBlock (running->done
        # flip keyed by tool_call_id), not the generic CognitiveBlock.
        bridge.dispatch({"type": "message", "message_kind": "cognitive",
                         "text": "🔧 edit · ok",
                         "metadata": {"cog_type": "tool_call", "cog_event_id": "e1",
                                      "_inbound_event_id": "in1",
                                      "data": {"name": "edit", "status": "ok",
                                               "tool_call_id": "tc_e1"}}})
        await pilot.pause()
        assert len(app.query(ToolCallBlock)) == 1


@pytest.mark.asyncio
async def test_app_error_frame_shows_reason_not_fake_disconnect():
    # A gateway error frame arrives on a live socket; it must surface the reason
    # in the transcript and NOT flip the status bar to disconnected (which would
    # send the user chasing a connection problem that doesn't exist).
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.blocks import AgentReply
    from echo_agent.cli.tui.status_bar import StatusBar
    app = EchoTUI()
    async with app.run_test() as pilot:
        bar = app.query_one(StatusBar)
        before = len(app.query(AgentReply))
        app.on_error("rate limited")
        await pilot.pause()
        assert bar._ok is True  # 连接态未被误置断开
        replies = app.query(AgentReply)
        assert len(replies) == before + 1
        assert any("rate limited" in str(r.render()) for r in replies)


@pytest.mark.asyncio
async def test_status_bar_disconnect_text_is_honest():
    # No client-side reconnect exists, so the text must not claim "重试中".
    from echo_agent.cli.tui.status_bar import StatusBar
    bar = StatusBar()
    bar.set_connection(False)
    text = bar._compose_text()
    assert "重试" not in text
    assert "已断开" in text


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


@pytest.mark.asyncio
async def test_on_mount_writes_session_and_connected():
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.status_bar import StatusBar
    app = EchoTUI(session_key="cli:local")
    async with app.run_test() as pilot:
        await pilot.pause()
        bar = app.query_one(StatusBar)
        assert bar._ok is True
        text = str(bar.render())
        assert "●已连接" in text
        assert "cli:local" in text


@pytest.mark.asyncio
async def test_tool_block_toggles_on_click_and_enter():
    # The P2 fix: ToolCallBlock detail/diff was togglable in code but had no
    # user input path. It is now focusable and toggles on click / Enter.
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.blocks import ToolCallBlock
    from echo_agent.cli.tui.bridge import WSBridge
    app = EchoTUI()
    async with app.run_test() as pilot:
        bridge = WSBridge(app)
        bridge.dispatch({"type": "message", "message_kind": "cognitive",
                         "text": "🔧 edit", "metadata": {
                             "cog_type": "tool_call", "cog_event_id": "e1",
                             "_inbound_event_id": "in1",
                             "data": {"name": "edit_file", "status": "ok",
                                      "params": {"path": "a.py"},
                                      "result_text": "内容预览",
                                      "tool_call_id": "tc1"}}})
        await pilot.pause()
        blk = app.query_one(ToolCallBlock)
        assert blk.can_focus is True
        assert blk.expanded is False
        blk.focus()
        await pilot.pause()
        await pilot.press("enter")
        assert blk.expanded is True
        blk.on_click()   # mouse click toggles back
        assert blk.expanded is False


@pytest.mark.asyncio
async def test_approval_sequence_keeps_original_turn_active():
    """P0 end-to-end: while a turn is parked in approval, the redundant approval
    prompt text must NOT end the turn, and the /approve reply's accepted frame
    must NOT steal the interrupt target. Ctrl+C must still target the original
    turn until its own final reply lands."""
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.bridge import WSBridge
    from echo_agent.cli.tui.blocks import ApprovalBlock
    from echo_agent.cli.tui.status_bar import StatusBar
    sent = []
    interrupts = []
    async def fake_send(text): sent.append(text)
    async def fake_interrupt(eid=""): interrupts.append(eid)
    app = EchoTUI(send_coro=fake_send, interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        bridge = WSBridge(app)
        bar = app.query_one(StatusBar)

        # 1. User submits a real turn → accepted with the turn's event id.
        from echo_agent.cli.tui.prompt_input import PromptInput
        pi = app.query_one(PromptInput)
        pi.text = "帮我删除临时文件"
        await pilot.press("enter")
        await pilot.pause()
        bridge.dispatch({"type": "accepted", "event_id": "turn-1"})
        assert app._turns.active_turn_id == "turn-1"
        assert bar.is_turn_active is True

        # 2. Server sends the redundant approval-prompt TEXT (is_final, tagged
        #    _approval_request) — must be ignored, not end the turn.
        bridge.dispatch({"type": "message", "message_kind": "final",
                         "text": "⚠️ 需要确认执行 /approve req1",
                         "is_final": True,
                         "metadata": {"_inbound_event_id": "turn-1",
                                      "_approval_request": True}})
        await pilot.pause()
        assert bar.is_turn_active is True             # turn NOT ended
        assert app._turns.active_turn_id == "turn-1"

        # 3. The interactive approval frame renders the ApprovalBlock.
        app.on_cognitive(CogEvent("approval_request", "cog1", "turn-1",
                                  {"request_id": "req1", "action": "exec",
                                   "params": {}, "risk": "EXEC"}, "确认"))
        await pilot.pause()
        assert app.query_one(ApprovalBlock) is not None

        # 4. User approves (y) → /approve sent, gets its OWN accepted frame.
        await pilot.press("y")
        await pilot.pause()
        assert sent[-1] == "/approve req1"
        bridge.dispatch({"type": "accepted", "event_id": "approve-evt"})
        # The approval reply's id must NOT become the interrupt target.
        assert app._turns.active_turn_id == "turn-1"
        assert bar.is_turn_active is True

        # 5. Ctrl+C now must interrupt the ORIGINAL turn, not the approval reply.
        await pilot.press("ctrl+c")
        await pilot.pause()
        assert interrupts == ["turn-1"]

        # 6. Only the original turn's own final reply ends the turn.
        bridge.dispatch({"type": "message", "message_kind": "final",
                         "text": "已完成", "is_final": True,
                         "metadata": {"_inbound_event_id": "turn-1"}})
        await pilot.pause()
        assert bar.is_turn_active is False


@pytest.mark.asyncio
async def test_queued_second_turn_does_not_steal_interrupt_target():
    """P0: a second turn submitted while the first runs queues behind it; Ctrl+C
    still targets the running (first) turn."""
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.bridge import WSBridge
    from echo_agent.cli.tui.prompt_input import PromptInput
    interrupts = []
    async def fake_send(text): pass
    async def fake_interrupt(eid=""): interrupts.append(eid)
    app = EchoTUI(send_coro=fake_send, interrupt_coro=fake_interrupt)
    async with app.run_test() as pilot:
        bridge = WSBridge(app)
        pi = app.query_one(PromptInput)
        pi.text = "任务一"
        await pilot.press("enter")
        await pilot.pause()
        bridge.dispatch({"type": "accepted", "event_id": "turn-1"})
        pi.text = "任务二"
        # Queuing a second turn while the first runs now needs a confirming
        # second Enter (queue-guard); the first Enter only arms the window.
        await pilot.press("enter")
        await pilot.pause()
        await pilot.press("enter")
        await pilot.pause()
        bridge.dispatch({"type": "accepted", "event_id": "turn-2"})
        # Ctrl+C targets the running (oldest) turn.
        await pilot.press("ctrl+c")
        await pilot.pause()
        assert interrupts == ["turn-1"]


@pytest.mark.asyncio
async def test_disconnect_blocks_conversation_but_allows_reconnect():
    """After a drop, a conversation turn is refused (would be silently lost);
    /reconnect is still reachable and restores the connected state."""
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.prompt_input import PromptInput
    from echo_agent.cli.tui.status_bar import StatusBar
    sent = []
    reconnected = {"n": 0}
    async def fake_send(text): sent.append(text)
    async def fake_reconnect():
        reconnected["n"] += 1
        return True
    app = EchoTUI(send_coro=fake_send, reconnect_coro=fake_reconnect)
    async with app.run_test() as pilot:
        app.notify_disconnected()
        await pilot.pause()
        assert app._connected is False
        # A conversation turn is refused while disconnected.
        pi = app.query_one(PromptInput)
        pi.text = "继续任务"
        await pilot.press("enter")
        await pilot.pause()
        assert sent == []
        # /reconnect works and restores state.
        pi.text = "/reconnect"
        await pilot.press("enter")
        await pilot.pause()
        assert reconnected["n"] == 1
        assert app._connected is True
        assert app.query_one(StatusBar)._ok is True
        # Now a turn goes through.
        pi.text = "继续任务"
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["继续任务"]


@pytest.mark.asyncio
async def test_reconnect_failure_keeps_disconnected():
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.prompt_input import PromptInput
    async def fake_send(text): pass
    async def fake_reconnect():
        return False
    app = EchoTUI(send_coro=fake_send, reconnect_coro=fake_reconnect)
    async with app.run_test() as pilot:
        app.notify_disconnected()
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "/reconnect"
        await pilot.press("enter")
        await pilot.pause()
        assert app._connected is False


