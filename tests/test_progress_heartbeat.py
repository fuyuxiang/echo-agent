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
    assert ev.metadata["_hb_milestone"] == activity.milestone_seq
    assert ev.metadata["_hb_milestone"] >= 1
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


def test_enter_tool_same_phase_does_not_double_increment():
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")        # thinking -> calling_tool, seq 0 -> 1
    assert st.milestone_seq == 1
    st.enter_tool("web_search")        # already calling_tool, must NOT bump
    assert st.milestone_seq == 1
    st.enter_tool("other_tool")        # still calling_tool, must NOT bump
    assert st.milestone_seq == 1


def test_set_generating_is_idempotent():
    st = SharedActivityState(started_at=0.0)
    st.set_generating()                # thinking -> generating, seq 0 -> 1
    assert st.milestone_seq == 1
    st.set_generating()                # already generating, must NOT bump
    assert st.milestone_seq == 1


class _MsBus:
    def __init__(self):
        self.published = []

    async def publish_outbound(self, event):
        self.published.append(event)


class _MsEvent:
    channel = "weixin"
    chat_id = "u1"
    reply_to_id = None
    event_id = "evt-1"


class _MsCfg:
    enabled = True
    first_delay_sec = 0
    min_interval_sec = 0
    template = "⏳ {activity}（已用时 {elapsed}）"


@pytest.mark.asyncio
async def test_no_milestone_means_no_beat():
    bus = _MsBus()
    hb = ProgressHeartbeat(bus, _MsEvent(), _MsCfg())
    st = SharedActivityState(started_at=0.0)  # never bumps milestone_seq
    # Drive one decision cycle directly rather than sleeping.
    assert hb._should_beat(st) is False
    assert bus.published == []


@pytest.mark.asyncio
async def test_new_milestone_triggers_beat_and_records():
    bus = _MsBus()
    hb = ProgressHeartbeat(bus, _MsEvent(), _MsCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # milestone_seq -> 1
    assert hb._should_beat(st) is True
    await hb._publish(st)
    assert len(bus.published) == 1
    assert bus.published[0].metadata["_hb_milestone"] == 1
    # After delivery the consumer records last_delivered_milestone; once equal,
    # no further beat until a new milestone arrives.
    st.last_delivered_milestone = 1
    assert hb._should_beat(st) is False


class _ThrottleCfg:
    enabled = True
    first_delay_sec = 0
    min_interval_sec = 60  # throttling ON
    template = "⏳ {activity}（已用时 {elapsed}）"


def test_silence_gate_blocks_when_recent_feedback():
    # Milestone advanced (passes progress gate) but last visible feedback is
    # too recent (fails silence gate) -> no beat. Use a non-key milestone so the
    # key-milestone throttle bypass (I1) does not apply here.
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _ThrottleCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # first entry: key milestone
    st.exit_tool()               # milestone_seq -> 2, non-key, passes progress gate
    st.last_visible_feedback_at = time.monotonic()  # very recent
    assert hb._should_beat(st) is False  # silence gate blocks


def test_silence_gate_allows_after_interval():
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _ThrottleCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # first entry: key milestone
    st.exit_tool()               # non-key milestone, passes progress gate
    st.last_visible_feedback_at = time.monotonic() - 120  # 2 min ago, > interval
    assert hb._should_beat(st) is True


class _LegacyIntervalCfg:
    # No min_interval_sec; only the legacy interval_sec name (rename transition).
    enabled = True
    first_delay_sec = 0
    interval_sec = 60
    template = "⏳ {activity}（已用时 {elapsed}）"


def test_silence_gate_reads_legacy_interval_sec():
    # min_interval_sec absent -> fall back to interval_sec for throttling. Use a
    # non-key milestone so we exercise the throttle, not the key bypass (I1).
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _LegacyIntervalCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # first entry: key milestone
    st.exit_tool()               # non-key milestone, passes progress gate
    st.last_visible_feedback_at = time.monotonic()  # very recent
    assert hb._should_beat(st) is False  # throttled via legacy interval_sec
    st.last_visible_feedback_at = time.monotonic() - 120  # > interval
    assert hb._should_beat(st) is True


# --- I1: key milestones bypass the min_interval throttle ---


def test_key_milestone_bypasses_throttle():
    # Recent visible feedback inside the throttle window WOULD normally block,
    # but a key milestone (first tool entry) must beat anyway.
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _ThrottleCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # first tool entry -> key milestone, seq 1
    assert st.last_milestone_is_key is True
    st.last_visible_feedback_at = time.monotonic()  # very recent, within window
    assert hb._should_beat(st) is True  # key milestone is not throttled


def test_non_key_milestone_still_throttled():
    # A non-key milestone in the same throttle window must stay throttled,
    # proving the bypass is scoped to key milestones only.
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _ThrottleCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # first entry, key
    st.exit_tool()               # seq 2, NOT key
    assert st.last_milestone_is_key is False
    st.last_visible_feedback_at = time.monotonic()  # recent, within window
    assert hb._should_beat(st) is False  # ordinary milestone still throttled


def test_finalize_key_milestone_bypasses_throttle():
    # The "entering finalize" (generating) key milestone must not be swallowed
    # by the throttle even right after a recent beat.
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _ThrottleCfg())
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # key, seq 1
    st.exit_tool()               # non-key, seq 2
    st.last_delivered_milestone = 2
    st.set_generating()          # entering finalize -> key, seq 3
    assert st.last_milestone_is_key is True
    st.last_visible_feedback_at = time.monotonic()  # recent
    assert hb._should_beat(st) is True


# --- I2: source-side gate advances after delivery (no rebeat of same milestone) ---


def test_source_gate_advances_and_blocks_rebeat():
    # Drive one beat cycle the way _run does: a new milestone beats once, then
    # the source advances last_delivered_milestone so the SAME milestone does
    # not rebeat; only a genuinely new milestone re-opens the gate.
    hb = ProgressHeartbeat(_MsBus(), _MsEvent(), _MsCfg())  # throttle disabled
    st = SharedActivityState(started_at=0.0)
    st.enter_tool("web_search")  # seq 1
    assert hb._should_beat(st) is True
    # Simulate _run delivering and advancing the source gate.
    st.last_delivered_milestone = st.milestone_seq
    assert hb._should_beat(st) is False  # same milestone must not rebeat
    st.exit_tool()  # seq 2 -> new milestone re-opens the gate
    assert hb._should_beat(st) is True


@pytest.mark.asyncio
async def test_run_loop_advances_source_gate_no_rebeat():
    # Integration: the real _run loop must advance last_delivered_milestone on
    # its own (production code path), so a single milestone yields exactly one
    # beat even though the loop ticks many times.
    bus = _MsBus()
    hb = ProgressHeartbeat(bus, _MsEvent(), _MsCfg())  # first_delay 0, throttle off
    st = SharedActivityState(started_at=time.monotonic())
    st.exit_tool()  # non-key milestone, seq 1 (avoids key-bypass re-beating)
    await hb.start(st)
    await asyncio.sleep(0.05)  # many ticks at _TICK_SEC granularity
    await hb.stop()
    assert len(bus.published) == 1  # exactly one beat, not one-per-tick
    assert st.last_delivered_milestone == 1

