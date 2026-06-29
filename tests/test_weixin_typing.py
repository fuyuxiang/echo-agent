"""Tests for the Weixin typing indicator (ilink/bot/getconfig + sendtyping).

Covers:
  1. typing_ticket fetch + caching via getconfig
  2. _do_send_typing wiring (status + ticket passed through)
  3. _start_typing / _stop_typing lifecycle: emits start then a final stop
  4. config toggle and send() stopping the indicator on reply
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels import weixin as wx
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.config.schema import WeixinChannelConfig


def _make_weixin(tmp_path: Path, *, typing_indicator: bool = True) -> WeixinChannel:
    cfg = WeixinChannelConfig(
        account_id="acct@im.bot",
        token="acct@im.bot:tok",
        data_dir=str(tmp_path / "weixin"),
        typing_indicator=typing_indicator,
    )
    ch = WeixinChannel(cfg, MessageBus())
    ch._send_session = MagicMock()  # truthy; real calls are monkeypatched
    return ch


def _patch_getconfig(monkeypatch, ticket: str | None) -> list[int]:
    calls: list[int] = []

    async def fake_getconfig(session, *, base_url, token):
        calls.append(1)
        return {"typing_ticket": ticket} if ticket else {}

    monkeypatch.setattr(wx, "_get_config", fake_getconfig)
    return calls


class TestTypingTicket:
    @pytest.mark.asyncio
    async def test_fetches_and_caches_ticket(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        calls = _patch_getconfig(monkeypatch, "T1")
        first = await ch._ensure_typing_ticket()
        second = await ch._ensure_typing_ticket()
        assert first == "T1"
        assert second == "T1"
        assert len(calls) == 1  # second call served from cache

    @pytest.mark.asyncio
    async def test_none_when_getconfig_has_no_ticket(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        _patch_getconfig(monkeypatch, None)
        assert await ch._ensure_typing_ticket() is None

    @pytest.mark.asyncio
    async def test_none_when_getconfig_raises(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)

        async def boom(session, *, base_url, token):
            raise RuntimeError("network down")

        monkeypatch.setattr(wx, "_get_config", boom)
        assert await ch._ensure_typing_ticket() is None


class TestSendTyping:
    @pytest.mark.asyncio
    async def test_do_send_typing_passes_status_and_ticket(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        _patch_getconfig(monkeypatch, "TKT")
        sent: list[tuple] = []

        async def fake_sendtyping(session, *, base_url, token, to, status, typing_ticket):
            sent.append((to, status, typing_ticket))
            return {}

        monkeypatch.setattr(wx, "_send_typing", fake_sendtyping)
        await ch._do_send_typing("user@x", wx._TYPING_START)
        assert sent == [("user@x", wx._TYPING_START, "TKT")]

    @pytest.mark.asyncio
    async def test_do_send_typing_skips_without_ticket(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        _patch_getconfig(monkeypatch, None)
        called = False

        async def fake_sendtyping(session, **kwargs):
            nonlocal called
            called = True
            return {}

        monkeypatch.setattr(wx, "_send_typing", fake_sendtyping)
        await ch._do_send_typing("user@x", wx._TYPING_START)
        assert called is False


class TestTypingLifecycle:
    @pytest.mark.asyncio
    async def test_start_then_stop_emits_start_and_final_stop(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        monkeypatch.setattr(wx, "_TYPING_REFRESH_INTERVAL", 0.01)
        _patch_getconfig(monkeypatch, "TKT")
        sent: list[int] = []

        async def fake_sendtyping(session, *, base_url, token, to, status, typing_ticket):
            sent.append(status)
            return {}

        monkeypatch.setattr(wx, "_send_typing", fake_sendtyping)

        ch._start_typing("user@x")
        await asyncio.sleep(0.03)  # allow a couple of start refreshes
        ch._stop_typing("user@x")
        await asyncio.sleep(0.02)  # allow the loop's finally to run

        assert wx._TYPING_START in sent
        assert sent[-1] == wx._TYPING_STOP
        assert "user@x" not in ch._typing_tasks

    @pytest.mark.asyncio
    async def test_start_is_idempotent_per_chat(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        monkeypatch.setattr(wx, "_TYPING_REFRESH_INTERVAL", 0.01)
        _patch_getconfig(monkeypatch, "TKT")
        monkeypatch.setattr(wx, "_send_typing", AsyncMock(return_value={}))

        ch._start_typing("user@x")
        task1 = ch._typing_tasks["user@x"]
        ch._start_typing("user@x")
        task2 = ch._typing_tasks["user@x"]
        assert task1 is task2  # no duplicate loop
        ch._stop_typing("user@x")
        await asyncio.sleep(0.02)

    @pytest.mark.asyncio
    async def test_disabled_does_not_start(self, tmp_path):
        ch = _make_weixin(tmp_path, typing_indicator=False)
        ch._start_typing("user@x")
        assert ch._typing_tasks == {}


class TestSendStopsTyping:
    @pytest.mark.asyncio
    async def test_send_stops_typing_for_chat(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        ch._send_text = AsyncMock(return_value=wx.SendResult(success=True))
        stopped: list[str] = []
        monkeypatch.setattr(ch, "_stop_typing", lambda cid: stopped.append(cid))

        ev = OutboundEvent.text_reply(channel="weixin", chat_id="user@x", text="hi")
        res = await ch.send(ev)

        assert res.success
        assert stopped == ["user@x"]
