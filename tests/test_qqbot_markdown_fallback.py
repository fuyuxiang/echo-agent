"""QQBot auto-downgrades to plain text when native markdown is not permitted.

QQ returns no stable error code for "markdown not permitted" — it replies with
plain-text words. The channel probes markdown once per target, and on denial
retries the same message as plain text and caches the verdict so later messages
skip markdown entirely (until the TTL expires).
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from echo_agent.channels.qqbot import QQBotChannel, _MD_UNSUPPORTED_TTL


def _make_channel(*, markdown: bool = True) -> QQBotChannel:
    from echo_agent.bus.queue import MessageBus

    cfg = MagicMock()
    cfg.app_id = "x"
    cfg.app_secret = "y"
    cfg.sandbox = False
    cfg.markdown_support = markdown
    cfg.media_enabled = False
    cfg.media_parse_tags = False
    cfg.media_max_file_size_mb = 10
    cfg.media_upload_cache_size = 4
    ch = QQBotChannel(cfg, MessageBus())
    ch._access_token = "tok"
    ch._token_expires = 9e18
    return ch


class _FakeResp:
    def __init__(self, status: int, body: str = "") -> None:
        self.status = status
        self._body = body

    async def text(self) -> str:
        return self._body

    async def __aenter__(self) -> "_FakeResp":
        return self

    async def __aexit__(self, *exc) -> None:
        return None


def _install_session(ch: QQBotChannel, responses: list[_FakeResp]) -> list[dict]:
    """Wire ch._session.post to yield queued responses; capture sent payloads."""
    sent_payloads: list[dict] = []
    it = iter(responses)

    def post(url, json, headers):
        sent_payloads.append(json)
        return next(it)

    ch._session = MagicMock()
    ch._session.post = post
    return sent_payloads


@pytest.mark.asyncio
async def test_denied_markdown_downgrades_to_plain() -> None:
    ch = _make_channel(markdown=True)
    payloads = _install_session(
        ch,
        [
            _FakeResp(400, '{"message":"markdown not allowed"}'),  # md rejected
            _FakeResp(200),  # plain-text retry succeeds
        ],
    )
    ok = await ch._send_chunk("g1", "hello **world**", "group", "")
    assert ok is True
    # First attempt used markdown, second downgraded to plain text.
    assert payloads[0]["msg_type"] == 2
    assert payloads[1]["msg_type"] == 0
    # Inline markers stripped in the downgraded plain-text body.
    assert payloads[1]["content"] == "hello world"


@pytest.mark.asyncio
async def test_denial_is_cached_per_target() -> None:
    ch = _make_channel(markdown=True)
    _install_session(
        ch,
        [_FakeResp(400, "native markdown 无权限"), _FakeResp(200)],
    )
    await ch._send_chunk("g1", "x", "group", "")
    assert "g1" in ch._md_unsupported
    # Later sends to the same target skip markdown up front.
    assert ch._markdown_allowed("g1", "group") is False
    # A different target is still probed with markdown.
    assert ch._markdown_allowed("g2", "group") is True


@pytest.mark.asyncio
async def test_cache_expiry_reprobes(monkeypatch) -> None:
    ch = _make_channel(markdown=True)
    base = 1000.0
    monkeypatch.setattr("echo_agent.channels.qqbot.time.time", lambda: base)
    ch._mark_markdown_unsupported("g1")
    assert ch._markdown_allowed("g1", "group") is False
    # Past the TTL, the target is re-probed with markdown.
    monkeypatch.setattr(
        "echo_agent.channels.qqbot.time.time", lambda: base + _MD_UNSUPPORTED_TTL + 1
    )
    assert ch._markdown_allowed("g1", "group") is True
    assert "g1" not in ch._md_unsupported


@pytest.mark.asyncio
async def test_unrelated_400_does_not_trigger_downgrade() -> None:
    ch = _make_channel(markdown=True)
    payloads = _install_session(
        ch,
        [_FakeResp(400, '{"message":"some other error"}')],
    )
    ok = await ch._send_chunk("g1", "x", "group", "")
    assert ok is False
    # No plain-text retry, no cache poisoning on unrelated errors.
    assert len(payloads) == 1
    assert "g1" not in ch._md_unsupported


@pytest.mark.asyncio
async def test_markdown_disabled_never_probes() -> None:
    ch = _make_channel(markdown=False)
    payloads = _install_session(ch, [_FakeResp(200)])
    ok = await ch._send_chunk("g1", "hi", "group", "")
    assert ok is True
    assert payloads[0]["msg_type"] == 0


@pytest.mark.asyncio
async def test_channel_type_never_uses_markdown() -> None:
    ch = _make_channel(markdown=True)
    assert ch._markdown_allowed("ch1", "channel") is False
