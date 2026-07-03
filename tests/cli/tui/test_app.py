from echo_agent.cli.tui.bridge import WSBridge


class Sink:
    def __init__(self):
        self.events = []
    def on_user_reply_token(self, i, t): self.events.append(("tok", i, t))
    def on_user_reply_final(self, i, t): self.events.append(("fin", i, t))
    def on_cognitive(self, ev): self.events.append(("cog", ev.cog_type, ev.cog_event_id))
    def on_error(self, m): self.events.append(("err", m))


def test_bridge_routes_cognitive_with_dedup():
    s = Sink(); b = WSBridge(s)
    frame = {"type": "message", "message_kind": "cognitive",
             "text": "召回 1 条", "metadata": {"cog_type": "memory_recalled",
             "cog_event_id": "e1", "_inbound_event_id": "in1", "data": {}}}
    b.dispatch(frame)
    b.dispatch(frame)  # 重复应被去重
    assert s.events == [("cog", "memory_recalled", "e1")]


def test_bridge_routes_error_and_final_text():
    s = Sink(); b = WSBridge(s)
    b.dispatch({"type": "error", "error": "boom"})
    b.dispatch({"type": "message", "message_kind": "final", "text": "答案",
                "is_final": True, "metadata": {"_inbound_event_id": "in1"}})
    assert ("err", "boom") in s.events
    assert ("fin", "in1", "答案") in s.events


def test_bridge_ignores_control_frames():
    s = Sink(); b = WSBridge(s)
    for t in ("accepted", "auth_ok", "pong"):
        b.dispatch({"type": t})
    assert s.events == []
