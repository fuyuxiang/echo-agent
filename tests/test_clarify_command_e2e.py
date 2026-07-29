import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider, LLMResponse


class _StubProvider(LLMProvider):
    def __init__(self):
        super().__init__()

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


def test_is_clarify_command_detection(tmp_path):
    loop = _make_loop(tmp_path)
    assert loop._is_clarify_command("/clarify c1 A") is True
    assert loop._is_clarify_command("/approve x") is False
    assert loop._is_clarify_command("普通消息") is False


@pytest.mark.asyncio
async def test_handle_clarify_resolves_pending(tmp_path):
    loop = _make_loop(tmp_path)
    req = loop.clarify.request("选哪个?", ["A", "B"], user_id="u1")

    async def wait_side():
        return await loop.clarify.wait_for_answer(req.id)

    import asyncio
    waiter = asyncio.create_task(wait_side())
    await asyncio.sleep(0.01)
    # A multi-word free-text answer must be preserved verbatim (split maxsplit=2).
    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1",
        text=f"/clarify {req.id} 方案 B 都行",
    )
    reply = await loop._handle_clarify_command(event)
    assert reply is not None
    answer, interrupted = await asyncio.wait_for(waiter, timeout=1.0)
    assert answer == "方案 B 都行"
    assert interrupted is False


@pytest.mark.asyncio
async def test_handle_clarify_unknown_id_is_friendly(tmp_path):
    loop = _make_loop(tmp_path)
    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1",
        text="/clarify nope X",
    )
    reply = await loop._handle_clarify_command(event)
    assert reply is not None
    assert "nope" in reply


@pytest.mark.asyncio
async def test_clarify_answer_delivered_to_waiter(tmp_path):
    import asyncio
    loop = _make_loop(tmp_path)
    req = loop.clarify.request("q", ["A"], user_id="u1")

    async def wait_side():
        return await loop.clarify.wait_for_answer(req.id)

    waiter = asyncio.create_task(wait_side())
    await asyncio.sleep(0.01)
    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1", text=f"/clarify {req.id} A",
    )
    await loop._handle_clarify_command(event)
    answer, interrupted = await asyncio.wait_for(waiter, timeout=1.0)
    assert answer == "A"
    assert interrupted is False


@pytest.mark.asyncio
async def test_handle_clarify_preserves_whitespace_only_answer(tmp_path):
    """Regression: split(maxsplit=2) collapsed "/clarify c1   " down to two
    tokens, so an answer made only of spaces arrived as "" — indistinguishable
    from "no answer argument". The model then learned nothing and re-asked the
    same question while the TUI already showed it as answered."""
    import asyncio
    loop = _make_loop(tmp_path)
    req = loop.clarify.request("q", ["A"], user_id="u1")

    waiter = asyncio.create_task(loop.clarify.wait_for_answer(req.id))
    await asyncio.sleep(0.01)
    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1",
        text=f"/clarify {req.id}   ",
    )
    reply = await loop._handle_clarify_command(event)
    assert reply is not None and "已回复" in reply
    answer, interrupted = await asyncio.wait_for(waiter, timeout=1.0)
    assert answer == "  "
    assert interrupted is False


@pytest.mark.asyncio
async def test_handle_clarify_missing_id_reports_usage(tmp_path):
    loop = _make_loop(tmp_path)
    for text in ("/clarify", "/clarify   "):
        event = InboundEvent.text_message(
            channel="gateway:cli", chat_id="c", sender_id="u1", text=text,
        )
        reply = await loop._handle_clarify_command(event)
        assert reply is not None and "用法" in reply


def test_cancel_command_detection(tmp_path):
    loop = _make_loop(tmp_path)
    assert loop._is_clarify_cancel_command("/__clarify_cancel__") is True
    assert loop._is_clarify_cancel_command("/clarify x y") is False
    assert loop._is_clarify_cancel_command("hi") is False


@pytest.mark.asyncio
async def test_cancel_command_interrupts_session_clarifies(tmp_path):
    import asyncio
    loop = _make_loop(tmp_path)
    req = loop.clarify.request("q", ["A"], user_id="u1", session_key="gateway:cli")

    async def wait_side():
        return await loop.clarify.wait_for_answer(req.id)

    waiter = asyncio.create_task(wait_side())
    await asyncio.sleep(0.01)
    event = InboundEvent.text_message(
        channel="gateway:cli", chat_id="c", sender_id="u1",
        text="/__clarify_cancel__",
    )
    event.session_key_override = "gateway:cli"
    assert loop._is_clarify_cancel_command(event.text) is True
    await loop._handle_clarify_cancel(event)
    answer, interrupted = await asyncio.wait_for(waiter, timeout=1.0)
    assert (answer, interrupted) == ("", True)
