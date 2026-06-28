import asyncio
import time
import pytest
from echo_agent.agent.progress_heartbeat import ProgressHeartbeat, SharedActivityState
from echo_agent.config.schema import HeartbeatConfig
from echo_agent.bus.events import InboundEvent


class _RecordingBus:
    def __init__(self):
        self.events = []
    async def publish_outbound(self, event):
        self.events.append(event)


@pytest.mark.asyncio
async def test_heartbeat_fires_during_long_work_and_sealed_after():
    """End-to-end of the timer contract used by AgentLoop: a long body
    yields heartbeats; once stop() is called no further beats appear."""
    bus = _RecordingBus()
    # interval_sec must be >= 1 per HeartbeatConfig schema (ge=1); use the
    # minimal valid value so the first beat still fires immediately after the
    # zero first_delay_sec gate.
    cfg = HeartbeatConfig(first_delay_sec=0, interval_sec=1)
    ev = InboundEvent(channel="cli", chat_id="c1")
    hb = ProgressHeartbeat(bus, ev, cfg)
    activity = SharedActivityState(started_at=time.monotonic())
    await hb.start(activity)
    await asyncio.sleep(0.05)            # simulate long work
    fired = len(bus.events)
    await hb.stop()                      # final answer -> seal
    await asyncio.sleep(0.05)
    assert fired >= 1
    assert len(bus.events) == fired      # no beats after seal
    assert all(e.message_kind == "heartbeat" for e in bus.events)
