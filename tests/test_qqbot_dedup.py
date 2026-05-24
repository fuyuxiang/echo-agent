"""Tests for QQBot dedup amortized O(1) and send-retry behavior."""

from __future__ import annotations

import time
from unittest.mock import MagicMock

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.channels.qqbot import QQBotChannel, _DEDUP_TTL


def _make_channel() -> QQBotChannel:
    cfg = MagicMock()
    cfg.app_id = "x"
    cfg.app_secret = "y"
    cfg.sandbox = False
    cfg.markdown_support = False
    cfg.media_enabled = False
    cfg.media_parse_tags = False
    cfg.media_max_file_size_mb = 10
    cfg.media_upload_cache_size = 4
    return QQBotChannel(cfg, MessageBus())


def test_is_duplicate_marks_seen_message() -> None:
    ch = _make_channel()
    assert ch._is_duplicate("m1") is False
    assert ch._is_duplicate("m1") is True
    assert ch._is_duplicate("m2") is False


def test_is_duplicate_evicts_stale_entries(monkeypatch) -> None:
    ch = _make_channel()
    base = 1000.0

    monkeypatch.setattr("echo_agent.channels.qqbot.time.time", lambda: base)
    ch._is_duplicate("old1")
    ch._is_duplicate("old2")

    # Jump past TTL — old entries must be swept lazily, not via O(n) rebuild.
    monkeypatch.setattr("echo_agent.channels.qqbot.time.time", lambda: base + _DEDUP_TTL + 1)
    assert ch._is_duplicate("new1") is False
    assert "old1" not in ch._seen_messages
    assert "old2" not in ch._seen_messages
    assert "new1" in ch._seen_messages


def test_is_duplicate_does_not_evict_fresh_entries(monkeypatch) -> None:
    """Fresh entries must NOT be discarded — the lazy sweep must stop at the
    first non-stale entry rather than rebuild the whole dict each call."""
    ch = _make_channel()
    base = 1000.0
    monkeypatch.setattr("echo_agent.channels.qqbot.time.time", lambda: base)
    for i in range(50):
        ch._is_duplicate(f"m{i}")

    # Advance only slightly — well within TTL.
    monkeypatch.setattr("echo_agent.channels.qqbot.time.time", lambda: base + 5)
    ch._is_duplicate("m_new")
    # All 50 prior entries are still tracked.
    for i in range(50):
        assert f"m{i}" in ch._seen_messages


# ── send retry budget — P2-E ─────────────────────────────────────────────────


class _FakeResp:
    def __init__(self, status: int, body: str = ""):
        self.status = status
        self._body = body

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        return False

    async def text(self) -> str:
        return self._body


class _FakeSession:
    def __init__(self, status_seq: list[tuple[int, str]]):
        self._seq = list(status_seq)
        self.posts: list[tuple[str, dict]] = []

    def post(self, url, json=None, headers=None):
        status, body = self._seq.pop(0) if self._seq else (200, "")
        self.posts.append((url, dict(json or {})))
        return _FakeResp(status, body)


@pytest.mark.asyncio
async def test_send_chunk_msg_id_expired_does_not_burn_retries(monkeypatch) -> None:
    """When QQ returns 40034024 (msg_id expired) the channel must drop msg_id
    and retry without consuming the regular retry budget. After dropping
    msg_id the next attempt should succeed on the same logical retry slot."""
    ch = _make_channel()
    # First response: 400 + 40034024 → drop msg_id, retry. Second: 200.
    fake = _FakeSession([
        (400, '{"code": 40034024, "message": "msg_id expired"}'),
        (200, ""),
    ])
    ch._session = fake  # type: ignore[assignment]

    # Refresh-token must not run for this test path.
    async def _no_refresh():
        return None

    ch._refresh_token = _no_refresh  # type: ignore[assignment]

    ok = await ch._send_chunk("chat1", "hello", "group", reply_to="rid")
    assert ok is True
    # Two real POSTs happened — first carried msg_id, second did not.
    assert len(fake.posts) == 2
    assert "msg_id" in fake.posts[0][1]
    assert "msg_id" not in fake.posts[1][1]


@pytest.mark.asyncio
async def test_send_chunk_401_triggers_refresh_and_retries() -> None:
    ch = _make_channel()
    fake = _FakeSession([(401, "unauthorized"), (200, "")])
    ch._session = fake  # type: ignore[assignment]

    refresh_calls = {"n": 0}

    async def _refresh():
        refresh_calls["n"] += 1

    ch._refresh_token = _refresh  # type: ignore[assignment]

    ok = await ch._send_chunk("chat1", "hi", "group", reply_to="")
    assert ok is True
    assert refresh_calls["n"] == 1


@pytest.mark.asyncio
async def test_send_chunk_caps_token_refresh_loop() -> None:
    """A misconfigured app_id that causes 401 forever must not loop forever —
    the channel caps automatic refresh attempts."""
    ch = _make_channel()
    fake = _FakeSession([(401, "x")] * 10)
    ch._session = fake  # type: ignore[assignment]

    refresh_calls = {"n": 0}

    async def _refresh():
        refresh_calls["n"] += 1

    ch._refresh_token = _refresh  # type: ignore[assignment]

    ok = await ch._send_chunk("chat1", "hi", "group", reply_to="")
    assert ok is False
    # Bounded refresh attempts — we shouldn't have hit it >5 times.
    assert refresh_calls["n"] <= 5
