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
    """End-to-end of the milestone contract used by AgentLoop: a long body that
    advances a milestone yields a heartbeat; once stop() is called no further
    beats appear."""
    bus = _RecordingBus()
    # min_interval_sec is the renamed throttle field; 1 is the minimal valid
    # value (schema ge=1) and the first beat still fires because no visible
    # feedback has happened yet.
    cfg = HeartbeatConfig(first_delay_sec=0, min_interval_sec=1)
    ev = InboundEvent(channel="cli", chat_id="c1")
    hb = ProgressHeartbeat(bus, ev, cfg)
    activity = SharedActivityState(started_at=time.monotonic())
    # A real milestone advances (tool entry) so the progress gate opens; pure
    # thinking would never bump the seq and would correctly emit zero beats.
    activity.enter_tool("web_search")
    await hb.start(activity)
    # Wait for the first beat by polling rather than a fixed sleep window: on a
    # loaded CI runner the freshly-scheduled _run task may not reach its first
    # iteration within a hardcoded 50ms, which previously made this flaky. The
    # 2s ceiling only bounds a genuine failure; the happy path resolves in ~1ms.
    for _ in range(200):
        if bus.events:
            break
        await asyncio.sleep(0.01)
    fired = len(bus.events)
    await hb.stop()                      # final answer -> seal
    await asyncio.sleep(0.05)
    assert fired >= 1
    assert len(bus.events) == fired      # no beats after seal
    assert all(e.message_kind == "heartbeat" for e in bus.events)
