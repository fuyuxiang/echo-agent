import asyncio
import time

import pytest

from echo_agent.agent.progress_heartbeat import (
    ActivitySnapshot,
    ProgressHeartbeat,
    SharedActivityState,
    format_elapsed,
    friendly_activity,
    render_heartbeat,
)
from echo_agent.bus.events import InboundEvent
from echo_agent.config.schema import HeartbeatConfig


def test_activity_starts_thinking():
    st = SharedActivityState(started_at=100.0)
    snap = st.snapshot(now=105.0)
    assert snap.phase == "thinking"
    assert snap.current_tool is None
    assert snap.elapsed_sec == pytest.approx(5.0)


def test_enter_and_exit_tool_tracks_phase():
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")
    snap = st.snapshot(now=1.0)
    assert snap.phase == "calling_tool"
    assert snap.current_tool == "web_search"
    st.exit_tool()
    snap2 = st.snapshot(now=2.0)
    assert snap2.phase == "thinking"
    assert snap2.current_tool is None


def test_set_generating_phase():
    st = SharedActivityState(started_at=0.0)
    st.set_generating()
    assert st.snapshot(now=1.0).phase == "generating"


def test_milestone_seq_increments_on_tool_transitions():
    st = SharedActivityState(started_at=0.0)
    assert st.milestone_seq == 0
    st.enter_tool("web_search")          # thinking -> calling_tool
    assert st.milestone_seq == 1
    st.exit_tool()                        # tool done
    assert st.milestone_seq == 2
    st.set_generating()                   # entering finalize
    assert st.milestone_seq == 3


def test_pure_generation_does_not_increment_milestone():
    st = SharedActivityState(started_at=0.0)
    # No tool calls, just sitting in thinking/generating for a long time.
    snap1 = st.snapshot(now=60.0)
    snap2 = st.snapshot(now=600.0)
    assert snap1.milestone_seq == 0
    assert snap2.milestone_seq == 0


def test_snapshot_carries_milestone_seq():
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("read_file")
    snap = st.snapshot(now=5.0)
    assert snap.milestone_seq == 1
    assert snap.phase == "calling_tool"
    assert snap.current_tool == "read_file"


def test_mark_visible_feedback_resets_since():
    st = SharedActivityState(started_at=0.0)
    base = time.monotonic()
    st.mark_visible_feedback(now=base)
    assert st.since_last_feedback(now=base + 7.0) == pytest.approx(7.0)


TEMPLATE = "⏳ 正在处理中… 已用时 {elapsed}（{activity}）"


def test_friendly_activity_maps_known_tool():
    snap = ActivitySnapshot(elapsed_sec=10, phase="calling_tool", current_tool="web_search")
    assert friendly_activity(snap) == "正在查阅资料"


def test_friendly_activity_unknown_tool_falls_back_no_leak():
    snap = ActivitySnapshot(elapsed_sec=10, phase="calling_tool", current_tool="super_secret_tool")
    out = friendly_activity(snap)
    assert out == "处理中"
    assert "super_secret_tool" not in out


def test_friendly_activity_thinking_and_generating():
    assert friendly_activity(ActivitySnapshot(1, "thinking", None)) == "思考中"
    assert friendly_activity(ActivitySnapshot(1, "generating", None)) == "正在组织答案"


def test_format_elapsed_seconds_and_minutes():
    assert format_elapsed(40) == "40 秒"
    assert format_elapsed(125) == "2 分钟"


def test_render_heartbeat_fills_template():
    snap = ActivitySnapshot(elapsed_sec=125, phase="calling_tool", current_tool="read_file")
    text = render_heartbeat(snap, TEMPLATE)
    assert text == "⏳ 正在处理中… 已用时 2 分钟（正在阅读文档）"


class _FakeBus:
    def __init__(self):
        self.events = []
    async def publish_outbound(self, event):
        self.events.append(event)


def _event():
    return InboundEvent(channel="cli", chat_id="c1", reply_to_id="r1")


def _cfg(**kw):
    return HeartbeatConfig(**{"first_delay_sec": 1, "interval_sec": 1, **kw})


@pytest.mark.asyncio
async def test_short_turn_emits_nothing():
    bus = _FakeBus()
    hb = ProgressHeartbeat(bus, _event(), _cfg(first_delay_sec=10))
    await hb.start(SharedActivityState(started_at=time.monotonic()))
    await asyncio.sleep(0.05)
    await hb.stop()
    assert bus.events == []


@pytest.mark.asyncio
async def test_long_turn_emits_heartbeat():
    bus = _FakeBus()
    hb = ProgressHeartbeat(bus, _event(), _cfg(first_delay_sec=0))
    activity = SharedActivityState(started_at=time.monotonic())
    activity.enter_tool("web_search")
    await hb.start(activity)
    await asyncio.sleep(0.05)
    await hb.stop()
    assert len(bus.events) >= 1
    ev = bus.events[0]
    assert ev.message_kind == "heartbeat"
    assert ev.is_final is False
    assert ev.metadata["_heartbeat"] is True
    assert "_inbound_event_id" in ev.metadata
    assert ev.metadata["_hb_on_uneditable"] in {"first_only", "off", "every"}
    assert "已用时" in ev.text and "查阅资料" in ev.text


@pytest.mark.asyncio
async def test_disabled_emits_nothing():
    bus = _FakeBus()
    hb = ProgressHeartbeat(bus, _event(), _cfg(enabled=False, first_delay_sec=0))
    await hb.start(SharedActivityState(started_at=time.monotonic()))
    await asyncio.sleep(0.05)
    await hb.stop()
    assert bus.events == []


@pytest.mark.asyncio
async def test_seal_after_stop_drops_late_heartbeat():
    bus = _FakeBus()
    hb = ProgressHeartbeat(bus, _event(), _cfg(first_delay_sec=0))
    await hb.start(SharedActivityState(started_at=time.monotonic()))
    await hb.stop()
    n = len(bus.events)
    await asyncio.sleep(0.05)
    assert len(bus.events) == n  # no new events after seal


@pytest.mark.asyncio
async def test_throttle_skips_when_recent_feedback():
    bus = _FakeBus()
    hb = ProgressHeartbeat(bus, _event(), _cfg(first_delay_sec=0, interval_sec=100))
    activity = SharedActivityState(started_at=time.monotonic())
    activity.mark_visible_feedback()  # recent -> within interval
    await hb.start(activity)
    await asyncio.sleep(0.05)
    await hb.stop()
    assert bus.events == []


@pytest.mark.asyncio
async def test_publish_exception_does_not_propagate():
    class _BoomBus:
        async def publish_outbound(self, event):
            raise RuntimeError("boom")
    hb = ProgressHeartbeat(_BoomBus(), _event(), _cfg(first_delay_sec=0))
    await hb.start(SharedActivityState(started_at=time.monotonic()))
    await asyncio.sleep(0.05)
    await hb.stop()  # must not raise
