"""End-to-end delivery receipt tests for ChannelManager.

Verifies that a channel's SendResult is surfaced through the global outbound
handler (_filter_and_dispatch) and aggregated by the bus into a real
DeliveryStage, rather than being silently dropped.
"""

import asyncio
from types import SimpleNamespace

import pytest

from echo_agent.agent.interrupt_manager import InterruptManager
from echo_agent.agent.loop import AgentLoop
from echo_agent.agent.streaming import ProcessResult
from echo_agent.bus.queue import MessageBus
from echo_agent.bus.events import (
    ContentBlock,
    ContentType,
    EventType,
    InboundEvent,
    OutboundEvent,
)
from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.base import BaseChannel, SendResult


class _StubConfig:
    """Minimal ChannelsConfig stand-in: only fields the manager reads."""

    send_progress = False
    send_tool_hints = False


class _FakeChannel(BaseChannel):
    name = "fake"

    def __init__(self, send_ok: bool):
        self._send_ok = send_ok
        self._running = True
        self.config = type("C", (), {"reactions_enabled": False})()

    async def start(self): ...

    async def stop(self): ...

    @property
    def is_running(self):
        return True

    async def send_typing(self, *a, **k): ...

    async def stop_typing(self, *a, **k): ...

    async def send(self, event: OutboundEvent) -> SendResult:
        return SendResult(
            success=self._send_ok,
            message_id="m1",
            error="" if self._send_ok else "platform down",
        )


def _final_event(channel="fake"):
    e = OutboundEvent(
        channel=channel,
        chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="hi")],
    )
    e.is_final = True
    e.message_kind = "final"
    return e


@pytest.mark.asyncio
async def test_failed_send_surfaces_as_failed_delivery():
    bus = MessageBus()
    mgr = ChannelManager(_StubConfig(), bus)
    mgr._channels["fake"] = _FakeChannel(send_ok=False)
    res = await bus.publish_outbound(_final_event())
    assert res.stage is DeliveryStage.FAILED
    assert "platform down" in (res.error or "")


@pytest.mark.asyncio
async def test_ok_send_surfaces_as_delivered():
    bus = MessageBus()
    mgr = ChannelManager(_StubConfig(), bus)
    mgr._channels["fake"] = _FakeChannel(send_ok=True)
    res = await bus.publish_outbound(_final_event())
    assert res.stage is DeliveryStage.DELIVERED


# --- Loop terminal writeback keyed on the real delivery receipt (Task A5) ---
#
# These reuse the __new__-based AgentLoop assembly style from
# test_task_outcome_writeback.py (bypassing __init__), wiring only the
# collaborators _on_inbound's delivery point touches. The bus is stubbed so
# publish_outbound returns a chosen DeliveryResult, and the terminal writeback
# hooks are captured — proving the loop reads the receipt instead of
# unconditionally recording "completed".


class _StubBus:
    """publish_outbound returns a fixed DeliveryResult and records every send."""

    def __init__(self, result: DeliveryResult):
        self._result = result
        self.sent: list[OutboundEvent] = []

    async def publish_outbound(self, event: OutboundEvent) -> DeliveryResult:
        self.sent.append(event)
        return self._result


class _StubSessions:
    def __init__(self):
        self._lock = asyncio.Lock()

    async def acquire(self, key: str) -> asyncio.Lock:
        return self._lock


class _StubTracer:
    def start_span(self, *a, **k):
        return SimpleNamespace()

    def end_span(self, *a, **k): ...

    def flush_trace(self, *a, **k): ...


def _cron_event() -> InboundEvent:
    return InboundEvent(
        event_type=EventType.CRON,
        channel="cron",
        chat_id="c1",
        sender_id="scheduler",
        content=[ContentBlock(type=ContentType.TEXT, text="run job")],
        metadata={"job_id": "job_1"},
    )


def _build_loop(delivery: DeliveryResult, result: ProcessResult):
    """Assemble a minimal AgentLoop whose _process_event yields `result` and
    whose bus returns `delivery`. Terminal writebacks are captured on the loop."""
    loop = AgentLoop.__new__(AgentLoop)
    loop._running = True
    loop.bus = _StubBus(delivery)
    loop.sessions = _StubSessions()
    loop.tracer = _StubTracer()
    loop.interrupt = InterruptManager()
    loop.cognitive_emitter = None
    loop.config = SimpleNamespace(
        session=SimpleNamespace(group_session_scope="shared"),
        agent=SimpleNamespace(heartbeat=SimpleNamespace(enabled=False)),
    )

    async def _fake_process_event(event, trace_id, **kw):
        return result

    loop._process_event = _fake_process_event

    cron_calls: list[tuple[str, str]] = []
    task_calls: list[tuple[str, str]] = []

    async def _fake_cron(event, status, error=""):
        cron_calls.append((status, error))

    async def _fake_task(event, status, error=""):
        task_calls.append((status, error))

    loop._record_cron_outcome = _fake_cron
    loop._record_task_outcome = _fake_task
    return loop, cron_calls, task_calls


@pytest.mark.asyncio
async def test_cron_records_error_when_delivery_failed():
    """End-to-end contract: a FAILED delivery receipt drives the CRON terminal
    state to "error", not "completed"."""
    loop, cron_calls, task_calls = _build_loop(
        delivery=DeliveryResult(DeliveryStage.FAILED, "cron", error="platform down"),
        result=ProcessResult(response_text="hi", outbound_sent=False),
    )

    await loop._on_inbound(_cron_event())

    assert cron_calls and cron_calls[-1][0] == "error"
    assert task_calls and task_calls[-1][0] == "error"


@pytest.mark.asyncio
async def test_cron_records_completed_when_delivery_ok():
    """A DELIVERED receipt keeps the clean-finish path: completed / completed."""
    loop, cron_calls, task_calls = _build_loop(
        delivery=DeliveryResult(DeliveryStage.DELIVERED, "cron"),
        result=ProcessResult(response_text="hi", outbound_sent=False),
    )

    await loop._on_inbound(_cron_event())

    assert cron_calls[-1] == ("completed", "")
    assert task_calls[-1] == ("completed", "")


@pytest.mark.asyncio
async def test_incomplete_turn_still_recorded_when_delivery_ok():
    """task_incomplete keeps recording incomplete when delivery succeeded."""
    loop, cron_calls, task_calls = _build_loop(
        delivery=DeliveryResult(DeliveryStage.DELIVERED, "cron"),
        result=ProcessResult(
            response_text="hi", outbound_sent=False, task_incomplete=True,
            termination_reason="budget_halted",
        ),
    )

    await loop._on_inbound(_cron_event())

    assert cron_calls[-1][0] == "completed"
    assert task_calls[-1][0] == "incomplete"
    assert loop.bus.sent[-1].metadata["_turn_status"] == "incomplete"
    # The turn answered, it just did not finish the task. `_error` drives
    # user-visible failure signals (the channel reaction emoji), so an
    # unfinished-but-answered turn must NOT set it; the reason is retained for
    # diagnostics and the status stays 200 because a reply exists.
    assert "_error" not in loop.bus.sent[-1].metadata
    assert loop.bus.sent[-1].metadata["_error_reason"] == "budget_halted"
    assert loop.bus.sent[-1].metadata["_http_status"] == 200


@pytest.mark.asyncio
async def test_interrupted_turn_final_frame_matches_terminal_status():
    loop, cron_calls, task_calls = _build_loop(
        delivery=DeliveryResult(DeliveryStage.DELIVERED, "cron"),
        result=ProcessResult(
            response_text="stopped",
            outbound_sent=False,
            task_incomplete=True,
            termination_reason="interrupted",
        ),
    )

    await loop._on_inbound(_cron_event())

    assert cron_calls[-1][0] == "completed"
    assert task_calls[-1][0] == "incomplete"
    assert loop.bus.sent[-1].metadata["_turn_status"] == "interrupted"
    # "stopped" reached the user, so this is not an error frame — see the
    # incomplete case above.
    assert "_error" not in loop.bus.sent[-1].metadata
    assert loop.bus.sent[-1].metadata["_http_status"] == 200


@pytest.mark.asyncio
async def test_streamed_failed_delivery_falls_back_and_records_error():
    """A streamed turn whose finalize receipt FAILED reports outbound_sent=False
    (ResponseStage only sets it on an ok receipt), so the loop republishes
    response_text. When that republish also returns FAILED, the terminal state
    is error — the real streaming-failure path, no unreachable mirror field."""
    loop, cron_calls, task_calls = _build_loop(
        delivery=DeliveryResult(DeliveryStage.FAILED, "cron", error="platform down"),
        result=ProcessResult(response_text="hi", outbound_sent=False),
    )

    await loop._on_inbound(_cron_event())

    assert cron_calls[-1][0] == "error"
    assert task_calls[-1][0] == "error"
    # failed stream fell back to a single republish of response_text
    assert len(loop.bus.sent) == 1
    assert loop.bus.sent[0].content[0].text == "hi"
