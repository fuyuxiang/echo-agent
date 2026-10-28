"""Email channel reliability — the three long-standing bugs.

Reviewed issues this pins:

1. ``_send_smtp`` swallowed every exception into a log line. The SMTP
   path returned success even when the message was rejected, so cron
   runs and tasks were marked complete despite the email never going out.

2. ``_fetch_imap`` used ``conn.search(None, "UNSEEN")`` (sequence numbers)
   and stored them as if they were UIDs. Sequence numbers shift on
   EXPUNGE; the in-memory set was the only state, so every restart
   re-fetched every UNSEEN message.

3. The mark-as-processed guard ran *before* ``publish_inbound``. A
   transient bus failure silently dropped the message, and the in-memory
   UID set prevented any retry.
"""

from __future__ import annotations

import json
import smtplib
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.email import EmailChannel
from echo_agent.config.schema import EmailChannelConfig


@pytest.fixture(autouse=True)
def _isolate_email_state(tmp_path, monkeypatch):
    """Pin the channel's state file under tmp_path so tests don't read or
    write the machine-global ``~/.echo-agent/data/email_state.json``.

    Patches ``echo_agent.runtime_paths.echo_home`` (the function imports
    it lazily inside ``_resolve_state_path``, so the *import source* is
    where the monkey-patch has to land, not the local binding inside
    ``email``).
    """
    monkeypatch.setattr(
        "echo_agent.runtime_paths.echo_home",
        lambda: tmp_path,
    )


def _config(tmp_path: Path) -> EmailChannelConfig:
    return EmailChannelConfig(
        enabled=True,
        imap_host="imap.example.com",
        imap_port=993,
        smtp_host="smtp.example.com",
        smtp_port=587,
        username="bot@example.com",
        password="x",
        use_ssl=True,
        poll_interval_seconds=60,
        allow_from=["alice@example.com"],
    )


def _channel(tmp_path: Path, bus: MessageBus | None = None) -> EmailChannel:
    if bus is None:
        # Real MessageBus so ``publish_inbound`` returns an awaitable bool.
        # The mocks on top of it (subscribe_outbound, etc.) are unused for
        # these tests, but the bus's actual coroutine surface is needed.
        bus = MessageBus()
    return EmailChannel(_config(tmp_path), bus)


# ── 1. SMTP failure must surface ─────────────────────────────────────────────


@pytest.mark.asyncio
async def test_smtp_failure_raises_so_caller_sees_it(tmp_path):
    """The send helper must NOT swallow SMTP errors into a log line —
    returning success on a 535 or a network error would mark the
    originating cron run / task as complete despite the email never
    going out.
    """
    ch = _channel(tmp_path)

    def boom(*_a, **_k):
        raise smtplib.SMTPException("535 Authentication failed")

    with patch.object(ch, "_send_smtp", side_effect=boom):
        event = OutboundEvent.text_reply(
            channel="email", chat_id="alice@example.com", text="hello",
        )
        result = await ch.send(event)

    assert result is not None
    assert result.success is False, "SMTP failure must surface as a failure"
    assert "535" in result.error


@pytest.mark.asyncio
async def test_smtp_success_returns_true(tmp_path):
    ch = _channel(tmp_path)
    with patch.object(ch, "_send_smtp", return_value=None):
        event = OutboundEvent.text_reply(
            channel="email", chat_id="alice@example.com", text="hello",
        )
        result = await ch.send(event)
    assert result is not None and result.success is True


# ── 2. IMAP: UID SEARCH + persistent watermark ─────────────────────────────


def _make_uid_response(*uids: bytes):
    """Build the (status, [b'UID1 UID2 ...']) shape imaplib returns for SEARCH."""
    status = b"OK"
    payload = [b" ".join(uids)] if uids else [b""]
    return (status, payload)


def _make_fetch_response(uid: bytes, raw: bytes):
    """RFC822 fetch returns a list of body parts."""
    return (b"OK", [(b"1 (UID " + uid + b" RFC822 {11})", raw), b")"])


def _raw_email(from_addr: str = "alice@example.com", body: str = "hi") -> bytes:
    return (
        f"From: {from_addr}\r\n"
        f"Subject: test\r\n"
        f"Content-Type: text/plain; charset=utf-8\r\n"
        f"\r\n"
        f"{body}\r\n"
    ).encode("utf-8")


def _conn_with(responses: dict[str, list]) -> MagicMock:
    """Build an imaplib.IMAP4 mock with responses grouped by UID command."""
    conn = MagicMock()
    scripts = {method: iter(items) for method, items in responses.items()}

    def fake_uid(method, *_args):
        try:
            return next(scripts[method])
        except (KeyError, StopIteration):
            return (b"OK", [b""])

    conn.uid.side_effect = fake_uid
    conn.select.return_value = (b"OK", [b"1"])
    conn.logout.return_value = (b"BYE", [b""])
    conn.login.return_value = (b"OK", [b""])
    return conn


@pytest.mark.asyncio
async def test_fetch_uses_uid_search_not_sequence_search(tmp_path):
    """The fix is UID SEARCH. Verify the protocol call, not just the
    result — a regression to sequence SEARCH would be invisible from the
    return value alone.
    """
    ch = _channel(tmp_path)
    conn = _conn_with({
        "SEARCH": [_make_uid_response(b"101", b"102", b"103")],
        "FETCH": [_make_fetch_response(b"101", _raw_email())],
    })
    with patch("imaplib.IMAP4_SSL", return_value=conn):
        await _call_fetch(ch)

    # First argument to uid() must be "SEARCH" (UID SEARCH), not the
    # sequence-number variant (which would have no "UID" prefix in the
    # range argument).
    first_call = conn.uid.call_args_list[0]
    assert first_call.args[0] == "SEARCH", (
        f"expected uid('SEARCH', ...), got {first_call.args!r}"
    )
    # The search range must include "UID N:*" (UID SEARCH form), proving we
    # asked for UIDs above the watermark — not "1:*" which is a sequence
    # range.
    all_call_args = [a for call in conn.uid.call_args_list for a in call.args]
    assert "UID 1:*" in all_call_args, (
        f"expected 'UID 1:*' among the UID SEARCH args, got {all_call_args!r}"
    )


@pytest.mark.asyncio
async def test_persisted_watermark_is_loaded_and_advances(tmp_path):
    """Restarting the channel must not re-fetch previously seen UIDs —
    that's the whole point of persisting the high-water mark.
    """
    state_path = tmp_path / "data" / "email_state.json"
    state_path.parent.mkdir(parents=True, exist_ok=True)
    state_path.write_text('{"last_seen_uid": 200}', encoding="utf-8")

    ch = _channel(tmp_path)
    assert ch._last_seen_uid == 200

    conn = _conn_with({
        "SEARCH": [_make_uid_response(b"201", b"202")],
        "FETCH": [
            _make_fetch_response(b"201", _raw_email()),
            _make_fetch_response(b"202", _raw_email()),
        ],
    })
    with patch("imaplib.IMAP4_SSL", return_value=conn):
        results = await _call_fetch(ch)

    assert [message.uid for message in results] == [201, 202]
    # Fetching alone cannot acknowledge a publishable email.
    assert ch._last_seen_uid == 200

    await ch._process_fetched(results)

    assert ch._last_seen_uid == 202
    assert "UID 201:*" in conn.uid.call_args_list[0].args

    # And the watermark was persisted.
    saved = json.loads(state_path.read_text(encoding="utf-8"))
    assert saved == {"last_seen_uid": 202}


@pytest.mark.asyncio
async def test_watermark_advances_for_filtered_or_empty_messages(tmp_path):
    """A message outside ``allow_from`` (or with an empty body) must still
    advance the watermark. Otherwise the same UID would be re-scanned on
    every poll, pinning the watermark.
    """
    ch = _channel(tmp_path)
    conn = _conn_with({
        "SEARCH": [_make_uid_response(b"301", b"302")],
        "FETCH": [
            _make_fetch_response(b"301", _raw_email(from_addr="mallory@evil.com")),
            _make_fetch_response(b"302", _raw_email(body="")),  # empty body
        ],
    })
    with patch("imaplib.IMAP4_SSL", return_value=conn):
        messages = await _call_fetch(ch)

    assert [message.publish for message in messages] == [False, False]
    assert ch._last_seen_uid == 0

    await ch._process_fetched(messages)

    # Both UIDs are acknowledged, even though neither needed publication.
    assert ch._last_seen_uid == 302


# ── 3. Mark after publish ────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_publish_failure_does_not_advance_watermark(tmp_path):
    """If ``publish_inbound`` returns falsy (bus overloaded), the UID must
    be retried on the next poll, not silently dropped. We assert this by
    checking the watermark *before* and *after* the failed publish: it
    should be unchanged.
    """
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=False)
    bus.publish_outbound = AsyncMock()
    ch = _channel(tmp_path, bus)
    conn = _conn_with({
        "SEARCH": [_make_uid_response(b"401")],
        "FETCH": [_make_fetch_response(b"401", _raw_email())],
    })
    with patch("imaplib.IMAP4_SSL", return_value=conn):
        messages = await _call_fetch(ch)
    await ch._process_fetched(messages)

    assert ch._last_seen_uid == 0
    bus.publish_inbound.assert_awaited_once()
    assert not ch._state_path.exists()

    # The next poll starts at the same UID and succeeds.
    bus.publish_inbound.return_value = True
    retry_conn = _conn_with({
        "SEARCH": [_make_uid_response(b"401")],
        "FETCH": [_make_fetch_response(b"401", _raw_email())],
    })
    with patch("imaplib.IMAP4_SSL", return_value=retry_conn):
        retry_messages = await _call_fetch(ch)
    await ch._process_fetched(retry_messages)

    assert "UID 1:*" in retry_conn.uid.call_args_list[0].args
    assert ch._last_seen_uid == 401
    assert bus.publish_inbound.await_count == 2


@pytest.mark.asyncio
async def test_failed_publish_does_not_skip_later_uids(tmp_path):
    """The watermark is a contiguous prefix, never merely the largest UID.

    UID 400 is filtered and may be acknowledged. UID 401 must be published but
    the bus rejects it, so UID 402 must neither be published nor acknowledged.
    """
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=False)
    bus.publish_outbound = AsyncMock()
    ch = _channel(tmp_path, bus)
    conn = _conn_with({
        "SEARCH": [_make_uid_response(b"400", b"401", b"402")],
        "FETCH": [
            _make_fetch_response(b"400", _raw_email(from_addr="mallory@evil.com")),
            _make_fetch_response(b"401", _raw_email(body="first")),
            _make_fetch_response(b"402", _raw_email(body="second")),
        ],
    })
    with patch("imaplib.IMAP4_SSL", return_value=conn):
        messages = await _call_fetch(ch)
    await ch._process_fetched(messages)

    assert ch._last_seen_uid == 400
    assert bus.publish_inbound.await_count == 1
    saved = json.loads(ch._state_path.read_text(encoding="utf-8"))
    assert saved == {"last_seen_uid": 400}


@pytest.mark.asyncio
async def test_search_result_at_watermark_is_not_refetched(tmp_path):
    """Some IMAP servers resolve ``UID N:*`` backwards when N > max UID."""
    state_path = tmp_path / "data" / "email_state.json"
    state_path.parent.mkdir(parents=True, exist_ok=True)
    state_path.write_text('{"last_seen_uid": 500}', encoding="utf-8")
    ch = _channel(tmp_path)
    conn = _conn_with({"SEARCH": [_make_uid_response(b"500")]})

    with patch("imaplib.IMAP4_SSL", return_value=conn):
        messages = await _call_fetch(ch)

    assert messages == []
    assert not any(call.args[0] == "FETCH" for call in conn.uid.call_args_list)
    assert ch._last_seen_uid == 500


# ── helper ──────────────────────────────────────────────────────────────────


async def _call_fetch(ch: EmailChannel):
    loop = __import__("asyncio").get_running_loop()
    return await loop.run_in_executor(None, ch._fetch_imap)
