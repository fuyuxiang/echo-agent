"""Root-cause regression: on uneditable channels (weixin), inference emits
progress/tool events that the ChannelManager DROPS (no edit support). Before the
fix the inference stage still marked visible-feedback on the shared activity,
which suppressed the heartbeat's _should_beat throttle, so a long turn with
frequent tool calls produced ZERO visible heartbeats — the silent-weixin bug.

This drives the REAL InferenceStage._emit_tool_event / _emit_progress methods
(bound onto a minimal stub) so the fix is exercised end-to-end through the bus
and ChannelManager, not re-implemented inline.
"""

import time
from types import SimpleNamespace

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.agent.progress_heartbeat import ProgressHeartbeat, SharedActivityState
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig


class _UneditableChan:
    name = "weixin"
    supports_edit = False
    is_running = True
    should_deliver = BaseChannel.should_deliver

    def __init__(self):
        self.config = type("C", (), {"reactions_enabled": False})()
        self.sent = []
        self.typings = 0

    async def send(self, event):
        if not self.should_deliver(event):
            return SimpleNamespace(success=True, skipped=True, message_id="")
        self.sent.append(event.text)
        return SimpleNamespace(success=True, skipped=False, message_id="m1")

    async def send_typing(self, chat_id, metadata=None):
        self.typings += 1

    async def stop_typing(self, chat_id):
        pass


class _Stub:
    """Minimal holder that borrows the real emit methods + bus/config."""

    _emit_tool_event = InferenceStage._emit_tool_event
    _emit_progress = InferenceStage._emit_progress

    def __init__(self, bus):
        self._bus = bus
        # _emit_tool_event reads config.gateway.emit_progress_events
        self._config = SimpleNamespace(gateway=SimpleNamespace(emit_progress_events=True))


@pytest.mark.asyncio
async def test_dropped_tool_event_does_not_starve_heartbeat():
    bus = MessageBus()
    mgr = ChannelManager(ChannelsConfig(), bus)
    ch = _UneditableChan()
    mgr._channels["weixin"] = ch

    ev = InboundEvent(channel="weixin", chat_id="c1")
    activity = SharedActivityState(started_at=time.monotonic())
    ctx = PipelineContext(event=ev, session=object(), trace_id="t", publish_response=True)
    ctx.activity = activity

    stub = _Stub(bus)
    # One tool call's progress event — dropped on weixin (no edit support).
    await stub._emit_tool_event(ctx, {"_phase": "calling_tool"})
    assert ch.sent == []  # user saw nothing

    # The dropped event must NOT have counted as visible feedback.
    assert activity.last_visible_feedback_at == 0.0

    # So the heartbeat is free to fire its first beat.
    hb = ProgressHeartbeat(bus, ev, HeartbeatConfig(first_delay_sec=0, interval_sec=60))
    assert hb._should_beat(activity), "heartbeat starved by a dropped tool event"
