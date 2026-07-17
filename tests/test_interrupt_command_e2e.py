"""E2E-ish tests for the turn-interrupt control path in AgentLoop: detection of
the /__interrupt__ sentinel and _handle_interrupt flagging the running turn +
waking a parked clarify. Mirrors test_clarify_command_e2e.py's _make_loop."""

import asyncio

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider, LLMResponse


class _StubProvider(LLMProvider):
    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok", finish_reason="stop")

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        return await self.chat(messages, tools, model, tool_choice, **kwargs)

    def get_default_model(self):
        return "stub"


def _make_loop(tmp_path):
    config = load_config(overrides={"workspace": str(tmp_path)})
    bus = MessageBus()
    return AgentLoop(bus=bus, config=config, provider=_StubProvider(), workspace=tmp_path)


def test_is_interrupt_command_detection(tmp_path):
    loop = _make_loop(tmp_path)
    assert loop._is_interrupt_command("/__interrupt__") is True
    assert loop._is_interrupt_command("  /__interrupt__  ") is True
    assert loop._is_interrupt_command("/interrupt") is False
    assert loop._is_interrupt_command("停止") is False


@pytest.mark.asyncio
async def test_handle_interrupt_flags_running_turn(tmp_path):
    loop = _make_loop(tmp_path)
    # Register a running turn for the session, then send the interrupt.
    loop.interrupt.request("gateway:cli", "evt_1")
    assert loop.interrupt.is_interrupted("gateway:cli") is False

    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1", text="/__interrupt__",
    )
    event.session_key_override = "gateway:cli"
    await loop._handle_interrupt(event)
    assert loop.interrupt.is_interrupted("gateway:cli") is True


@pytest.mark.asyncio
async def test_handle_interrupt_also_wakes_parked_clarify(tmp_path):
    # A Ctrl+C while the agent is parked on a clarify must unblock it (return the
    # interrupted sentinel), not leave it waiting.
    loop = _make_loop(tmp_path)
    req = loop.clarify.request("q", ["A"], user_id="u1", session_key="gateway:cli")

    async def wait_side():
        return await loop.clarify.wait_for_answer(req.id)

    waiter = asyncio.create_task(wait_side())
    await asyncio.sleep(0.01)

    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1", text="/__interrupt__",
    )
    event.session_key_override = "gateway:cli"
    await loop._handle_interrupt(event)

    answer, interrupted = await asyncio.wait_for(waiter, timeout=1.0)
    assert (answer, interrupted) == ("", True)


@pytest.mark.asyncio
async def test_handle_interrupt_on_idle_session_is_noop(tmp_path):
    loop = _make_loop(tmp_path)
    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1", text="/__interrupt__",
    )
    event.session_key_override = "gateway:cli"
    # No running turn registered → must not raise, stays un-interrupted.
    await loop._handle_interrupt(event)
    assert loop.interrupt.is_interrupted("gateway:cli") is False
