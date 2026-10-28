import inspect

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.bus.events import InboundEvent


def test_stages_accept_cognitive_emitter():
    assert "cognitive_emitter" in inspect.signature(InferenceStage.__init__).parameters
    assert "cognitive_emitter" in inspect.signature(ContextStage.__init__).parameters


def test_approval_gate_accepts_cognitive_emitter():
    from echo_agent.agent.approval_gate import ApprovalGate
    assert "cognitive_emitter" in inspect.signature(ApprovalGate.__init__).parameters


class _CapEmitter:
    def __init__(self, active=True):
        self.calls = []
        self._active = active

    def active(self, event):
        return self._active

    async def emit(self, event, cog_type, data, summary):
        self.calls.append((cog_type, data, summary))


class _FakeEntry:
    """Mimics a real memory entry object (the actual (entry, score) call-site shape)."""

    def __init__(self, key, content, entry_id, source):
        self.key = key
        self.content = content
        self.id = entry_id
        self.source = source


@pytest.mark.asyncio
async def test_context_stage_emits_memory_recalled():
    # Build a minimal ContextStage and drive its recall-emission helper directly.
    cap = _CapEmitter()
    stage = ContextStage.__new__(ContextStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    # Dict form (brief contract): normalized via content/source/score keys.
    scored = [
        {"content": "喜欢深色主题", "source": "user_stated", "score": 0.91},
        {"content": "在上海", "source": "consolidated", "score": 0.7},
    ]
    await stage._emit_memory_recalled(ev, scored)
    assert cap.calls[0][0] == "memory_recalled"
    assert cap.calls[0][1]["items"][0]["source"] == "user_stated"
    assert cap.calls[0][1]["items"][0]["score"] == 0.91
    assert "2" in cap.calls[0][2]  # summary carries the count


@pytest.mark.asyncio
async def test_context_stage_emits_memory_recalled_tuple_form():
    # Real call-site form: (entry, score) tuples from filtered mem_items.
    cap = _CapEmitter()
    stage = ContextStage.__new__(ContextStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    scored = [
        (_FakeEntry("pref", "喜欢深色主题", "id1", "user_stated"), 0.88),
        (_FakeEntry("loc", "在上海", "id2", "legacy"), 0.5),
    ]
    await stage._emit_memory_recalled(ev, scored)
    item = cap.calls[0][1]["items"][0]
    assert cap.calls[0][0] == "memory_recalled"
    assert item["content"] == "喜欢深色主题"
    assert item["source"] == "user_stated"
    assert item["score"] == 0.88
    assert "2" in cap.calls[0][2]


@pytest.mark.asyncio
async def test_context_stage_recall_noop_without_emitter():
    stage = ContextStage.__new__(ContextStage)
    stage._cog = None
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    # No emitter and empty list must both be safe no-ops.
    await stage._emit_memory_recalled(ev, [{"content": "x"}])
    stage._cog = _CapEmitter()
    await stage._emit_memory_recalled(ev, [])
    assert stage._cog.calls == []


@pytest.mark.asyncio
async def test_inference_stage_emits_tool_call_and_cost():
    cap = _CapEmitter()
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(ev, "edit", {"path": "a.py"}, "ok", "已写入 3 行")
    await stage._emit_cost(ev, 1200, 0.012, 0.033)
    assert cap.calls[0][0] == "tool_call"
    assert cap.calls[0][1]["name"] == "edit"
    assert cap.calls[0][1]["status"] == "ok"
    assert cap.calls[0][1]["params"]["path"] == "a.py"
    assert cap.calls[1][0] == "cost_update"
    assert cap.calls[1][1]["total_cost"] == 0.033
    assert cap.calls[1][1]["turn_tokens"] == 1200


@pytest.mark.asyncio
async def test_emit_tool_call_carries_id_and_meta():
    # Task 6: the cognition frame must carry tool_call_id (pairs running↔done),
    # result_meta (producer-supplied counts) and duration_ms so the cli renders
    # a Chinese, in-progress-flipping tool line.
    cap = _CapEmitter()
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(
        ev, "read_file", {"path": "a.py"}, "ok", "line1\nline2",
        tool_call_id="tc_1", result_meta={"total_lines": 42}, duration_ms=7,
    )
    data = cap.calls[-1][1]
    assert data["tool_call_id"] == "tc_1"
    assert data["result_meta"] == {"total_lines": 42}
    assert data["status"] == "ok"
    assert data["name"] == "read_file"
    assert data["duration_ms"] == 7


@pytest.mark.asyncio
async def test_running_frame_precedes_done_same_id():
    # Task 7: a full tool execution emits two tool_call frames paired by the
    # same tool_call_id — running first, terminal status second. This locks the
    # running↔done id/order contract; the real execution wiring is verified
    # manually.
    cap = _CapEmitter()
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(
        ev, "read_file", {"path": "a.py"}, "running", "", tool_call_id="tc_9",
    )
    await stage._emit_tool_call(
        ev, "read_file", {"path": "a.py"}, "ok", "x", tool_call_id="tc_9",
        result_meta={"total_lines": 3},
    )
    frames = [c[1] for c in cap.calls if c[1].get("name") == "read_file"]
    assert [f["tool_call_id"] for f in frames] == ["tc_9", "tc_9"]
    assert [f["status"] for f in frames] == ["running", "ok"]


@pytest.mark.asyncio
async def test_inference_stage_cost_and_tool_noop_without_emitter():
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = None
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(ev, "edit", {"path": "a.py"}, "ok", "x")
    await stage._emit_cost(ev, 1200, 0.012, 0.033)  # must be safe no-ops


@pytest.mark.asyncio
async def test_inference_stage_emits_written_thinking_evolution():
    # Verifies the render/output shape of three _emit_* helpers when called
    # directly. IMPORTANT: only `thinking` is actually wired to a turn-in
    # call-site. `memory_written` and `evolution` are render-ready helpers with
    # NO turn-in emission call-site (not wired) — their real signals are
    # background/offline (turn-after memory review returns list[str] logs; the
    # evolution engine is an offline/independent-command path). This test only
    # asserts helper output correctness, NOT that these events are emitted in a
    # turn. See the "Follow-up" section in the design spec / plan.
    cap = _CapEmitter()
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_memory_written(
        ev, [{"content": "喜欢方案2", "source": "user_stated", "op": "write"}]
    )
    await stage._emit_thinking(ev, 3200, "先看配置再改")
    await stage._emit_evolution(ev, "candidate", "faster_search", "生成候选")
    types = [c[0] for c in cap.calls]
    assert types == ["memory_written", "thinking", "evolution"]
    assert cap.calls[0][1]["items"][0]["op"] == "write"
    assert cap.calls[0][1]["items"][0]["source"] == "user_stated"
    assert cap.calls[1][1]["duration_ms"] == 3200
    assert cap.calls[1][1]["text"] == "先看配置再改"
    assert cap.calls[2][1]["skill"] == "faster_search"
    assert cap.calls[2][1]["phase"] == "candidate"


@pytest.mark.asyncio
async def test_tool_call_skips_payload_when_inactive():
    """非活跃通道(IM)：active() 为假时不构造 payload、不 emit。"""
    cap = _CapEmitter(active=False)
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:weixin", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(ev, "edit", {"path": "a.py"}, "ok", "x")
    assert cap.calls == []


@pytest.mark.asyncio
async def test_memory_recalled_skips_build_when_inactive():
    """非活跃通道：memory_recalled 不进入切片循环也不 emit。"""
    stage = ContextStage.__new__(ContextStage)
    stage._cog = _CapEmitter(active=False)
    ev = InboundEvent.text_message(
        channel="gateway:weixin", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_memory_recalled(ev, [{"content": "x", "source": "user_stated"}])
    assert stage._cog.calls == []


@pytest.mark.asyncio
async def test_real_emitter_active_matches_channel():
    """真实 emitter 的 active() 只对 gateway:cli 为真。"""
    from echo_agent.agent.cognitive_emitter import CognitiveEmitter
    cli_ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    im_ev = InboundEvent.text_message(
        channel="gateway:weixin", sender_id="u", chat_id="c", text="hi"
    )
    assert CognitiveEmitter.active(cli_ev) is True
    assert CognitiveEmitter.active(im_ev) is False


@pytest.mark.asyncio
async def test_written_thinking_evolution_noop_without_emitter():
    # Guards the None-emitter / empty-items no-op safety of the three helpers.
    # Same caveat as above: `memory_written` and `evolution` are render-ready
    # but NOT wired to any turn-in call-site (only `thinking` is). This asserts
    # helper safety, not that these events fire in a turn. See spec/plan
    # "Follow-up".
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = None
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    # No emitter and empty items must both be safe no-ops.
    await stage._emit_memory_written(ev, [{"content": "x"}])
    await stage._emit_thinking(ev, 100, "x")
    await stage._emit_evolution(ev, "p", "s", "d")
    stage._cog = _CapEmitter()
    await stage._emit_memory_written(ev, [])  # empty items -> no call
    assert stage._cog.calls == []


@pytest.mark.asyncio
async def test_heartbeat_emits_cognitive_event():
    from echo_agent.agent.progress_heartbeat import (
        ProgressHeartbeat,
        SharedActivityState,
    )

    class _FakeBus:
        def __init__(self):
            self.outbound = []

        async def publish_outbound(self, out):
            self.outbound.append(out)

    class _HbConfig:
        enabled = True
        template = "还在处理中，已经 {elapsed}，正在{activity}"

    cap = _CapEmitter()
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    bus = _FakeBus()
    hb = ProgressHeartbeat(bus, ev, _HbConfig(), cognitive_emitter=cap)
    activity = SharedActivityState(started_at=0.0, phase="calling_tool", current_tool="search")
    await hb._publish(activity)
    # Existing heartbeat frame still published.
    assert len(bus.outbound) == 1
    # Cognitive heartbeat event emitted additively.
    assert cap.calls[0][0] == "heartbeat"
    assert cap.calls[0][1]["stage"] == "calling_tool"
    assert "note" in cap.calls[0][1]


@pytest.mark.asyncio
async def test_non_cli_channel_emits_nothing():
    """Zero-intrusion guard: a non-cli (wechat) session must produce ZERO
    cognitive frames. Uses a REAL CognitiveEmitter so the channel gate inside
    emit() is the thing under test — wechat channel -> gate drops everything."""
    from echo_agent.agent.cognitive_emitter import CognitiveEmitter

    class _CapBus:
        def __init__(self):
            self.out = []

        async def publish_outbound(self, o):
            self.out.append(o)

    bus = _CapBus()
    emitter = CognitiveEmitter(bus)
    stage = ContextStage.__new__(ContextStage)
    stage._cog = emitter
    ev = InboundEvent.text_message(
        channel="gateway:wechat", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_memory_recalled(
        ev, [{"content": "x", "source": "user_stated", "score": 1.0}]
    )
    # Gate lives in emitter.emit(); wechat channel -> nothing published.
    assert bus.out == []
