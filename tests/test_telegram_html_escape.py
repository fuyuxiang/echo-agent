"""Telegram HTML escaping — every metacharacter the LLM can emit must land.

Reviewer P1-8 included: ``parse_mode=HTML`` rejects anything that looks
like a tag, so a reply containing ``Foo<T>``, ``a & b`` or ``x > 0``
returns 400 from Telegram and the whole reply is dropped. The fix escapes
``<``, ``>``, ``&`` before each send so the reply always lands.

Tests pin both the unit (``_escape_html``) and the integration (the
payload that would actually be sent).
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.channels.telegram import TelegramChannel, _escape_html


# ── _escape_html unit ────────────────────────────────────────────────────────


@pytest.mark.parametrize("inp, want", [
    ("plain text", "plain text"),
    ("", ""),
    ("Foo<Bar>", "Foo&lt;Bar&gt;"),
    ("a & b", "a &amp; b"),
    ("if x > 0 and y < 1: pass", "if x &gt; 0 and y &lt; 1: pass"),
    # Unmatched brackets: real cause of the 400.
    ("a<b", "a&lt;b"),
    ("b>a", "b&gt;a"),
    # `<3` looks like an open tag to Telegram; escape preserves it as text.
    ("<3 you", "&lt;3 you"),
    # Caller-supplied `&amp;` becomes double-escaped. Acceptable: the
    # original text was clearly already through a passthrough, and the
    # visible result is still correct.
    ("&amp; already escaped", "&amp;amp; already escaped"),
])
def test_escape_html_cases(inp, want):
    assert _escape_html(inp) == want


# ── send() integration ───────────────────────────────────────────────────────


def _channel(api_response: dict | None = None) -> TelegramChannel:
    """Build a TelegramChannel whose HTTP layer is mocked to capture what
    would have been POSTed to the Telegram API."""
    channel = TelegramChannel.__new__(TelegramChannel)
    channel._session = MagicMock()
    if api_response is None:
        api_response = {"ok": True, "result": {"message_id": 1}}
    channel._api = AsyncMock(return_value=api_response)
    channel._chunk_text = lambda text, limit: [text]
    channel._running = True
    return channel


@pytest.mark.asyncio
async def test_send_escapes_html_metacharacters():
    """An LLM reply with code/shell output lands verbatim rather than
    triggering a Telegram 400."""
    captured: list[dict] = []
    channel = _channel()

    async def _capture(method, **kwargs):
        captured.append({"method": method, **kwargs})
        return {"ok": True, "result": {"message_id": 1}}

    channel._api = _capture
    event = OutboundEvent.text_reply(
        channel="telegram", chat_id="c", text="if x > 0 and Foo<T> pass & done",
    )
    result = await channel.send(event)

    assert result.success
    payload = captured[0]["json"]
    # All three metacharacters escaped; nothing left raw that Telegram
    # would reject.
    assert "<" not in payload["text"]
    assert ">" not in payload["text"]
    assert "& " not in payload["text"]  # raw `& ` from `Foo<T> pass & done`
    # And the escaped forms made it through.
    assert "&lt;" in payload["text"]
    assert "&gt;" in payload["text"]
    assert "&amp;" in payload["text"]


@pytest.mark.asyncio
async def test_send_opt_out_preserves_markup():
    """A caller that has formatted its own HTML can opt out via metadata."""
    captured: list[dict] = []
    channel = _channel()

    async def _capture(method, **kwargs):
        captured.append({"method": method, **kwargs})
        return {"ok": True, "result": {"message_id": 1}}

    channel._api = _capture
    event = OutboundEvent.text_reply(
        channel="telegram", chat_id="c", text="<b>bold</b>",
    )
    event.metadata["telegram_markup"] = True
    await channel.send(event)

    assert captured[0]["json"]["text"] == "<b>bold</b>"


@pytest.mark.asyncio
async def test_edit_message_also_escapes():
    captured: list[dict] = []
    channel = _channel()

    async def _capture(method, **kwargs):
        captured.append({"method": method, **kwargs})
        return {"ok": True, "result": {"message_id": 1}}

    channel._api = _capture
    await channel.edit_message("c", 1, "a & b < c")

    payload = captured[0]["json"]
    assert payload["text"] == "a &amp; b &lt; c"