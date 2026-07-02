"""Tests for the Weixin typing indicator (ilink/bot/getconfig + sendtyping).

Typing is exposed via the BaseChannel primitives send_typing / stop_typing and
driven by ChannelManager (inbound + every heartbeat beat → send_typing; final
reply → stop_typing). The channel keeps an internal refresh loop because the
WeChat input state expires faster than the heartbeat interval.

Covers:
  1. typing_ticket fetch + caching via getconfig
  2. _do_send_typing wiring (status + ticket passed through)
  3. send_typing / stop_typing lifecycle: emits start then a final stop
  4. config toggle; send() no longer drives typing (manager owns it)
  5. regressions: double-cancel still emits status=2; orphan-cap loop is
     revived by a subsequent send_typing (heartbeat beat)
"""

from __future__ import annotations

import asyncio
import time
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

    async def fake_getconfig(session, *, base_url, token, user_id, context_token=None):
        calls.append(1)
        return {"typing_ticket": ticket} if ticket else {}

    monkeypatch.setattr(wx, "_get_config", fake_getconfig)
    return calls


class TestTypingTicket:
    @pytest.mark.asyncio
    async def test_fetches_and_caches_ticket(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        calls = _patch_getconfig(monkeypatch, "T1")
        first = await ch._ensure_typing_ticket("user@x")
        second = await ch._ensure_typing_ticket("user@x")
        assert first == "T1"
        assert second == "T1"
        assert len(calls) == 1  # second call served from cache

    @pytest.mark.asyncio
    async def test_none_when_getconfig_has_no_ticket(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        _patch_getconfig(monkeypatch, None)
        assert await ch._ensure_typing_ticket("user@x") is None

    @pytest.mark.asyncio
    async def test_none_when_getconfig_raises(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)

        async def boom(session, *, base_url, token, user_id, context_token=None):
            raise RuntimeError("network down")

        monkeypatch.setattr(wx, "_get_config", boom)
        assert await ch._ensure_typing_ticket("user@x") is None

    @pytest.mark.asyncio
    async def test_ticket_is_cached_per_chat(self, tmp_path, monkeypatch):
        """Each chat gets its own ticket; iLink binds tickets to the peer."""
        ch = _make_weixin(tmp_path)
        seen: list[str] = []

        async def fake_getconfig(session, *, base_url, token, user_id, context_token=None):
            seen.append(user_id)
            return {"typing_ticket": f"TKT-{user_id}"}

        monkeypatch.setattr(wx, "_get_config", fake_getconfig)
        assert await ch._ensure_typing_ticket("alice@x") == "TKT-alice@x"
        assert await ch._ensure_typing_ticket("bob@x") == "TKT-bob@x"
        # Each distinct chat triggers exactly one fetch; repeats hit the cache.
        assert await ch._ensure_typing_ticket("alice@x") == "TKT-alice@x"
        assert seen == ["alice@x", "bob@x"]


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

        await ch.send_typing("user@x")
        await asyncio.sleep(0.03)  # allow a couple of start refreshes
        await ch.stop_typing("user@x")
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

        await ch.send_typing("user@x")
        task1 = ch._typing_tasks["user@x"]
        await ch.send_typing("user@x")
        task2 = ch._typing_tasks["user@x"]
        assert task1 is task2  # no duplicate loop
        await ch.stop_typing("user@x")
        await asyncio.sleep(0.02)

    @pytest.mark.asyncio
    async def test_disabled_does_not_start(self, tmp_path):
        ch = _make_weixin(tmp_path, typing_indicator=False)
        await ch.send_typing("user@x")
        assert ch._typing_tasks == {}


class TestTypingCancellationRegressions:
    @pytest.mark.asyncio
    async def test_double_cancel_still_emits_stop(self, tmp_path, monkeypatch):
        """stop_typing then a second cancel (channel stop) must not drop status=2.

        Without shielding the finally's stop-send, the second CancelledError aborts
        it and leaves "typing" stuck on the peer's screen.
        """
        ch = _make_weixin(tmp_path)
        monkeypatch.setattr(wx, "_TYPING_REFRESH_INTERVAL", 0.01)
        _patch_getconfig(monkeypatch, "TKT")
        sent: list[int] = []

        async def fake_sendtyping(session, *, base_url, token, to, status, typing_ticket):
            if status == wx._TYPING_STOP:
                await asyncio.sleep(0.02)  # a real suspension point during cleanup
            sent.append(status)
            return {}

        monkeypatch.setattr(wx, "_send_typing", fake_sendtyping)

        await ch.send_typing("user@x")
        await asyncio.sleep(0.03)
        task = ch._typing_tasks["user@x"]
        await ch.stop_typing("user@x")   # first cancel
        await asyncio.sleep(0)           # let loop enter finally's shielded stop
        task.cancel()                    # second cancel (e.g. channel stop())
        await asyncio.gather(task, return_exceptions=True)
        await asyncio.sleep(0.03)        # let the shielded stop-send finish

        assert sent[-1] == wx._TYPING_STOP  # status=2 survived the double cancel

    @pytest.mark.asyncio
    async def test_orphan_cap_loop_is_revived_by_next_send_typing(self, tmp_path, monkeypatch):
        """When the refresh loop self-terminates at the orphan cap, a later
        send_typing (heartbeat beat) must start a fresh loop rather than no-op."""
        ch = _make_weixin(tmp_path)
        monkeypatch.setattr(wx, "_TYPING_REFRESH_INTERVAL", 0.001)
        monkeypatch.setattr(wx, "_TYPING_MAX_DURATION", 0.0)  # cap already elapsed
        _patch_getconfig(monkeypatch, "TKT")
        monkeypatch.setattr(wx, "_send_typing", AsyncMock(return_value={}))

        await ch.send_typing("user@x")
        await asyncio.sleep(0.01)  # loop hits cap, self-terminates, clears slot
        assert "user@x" not in ch._typing_tasks  # _on_typing_done cleared it

        # A subsequent beat must revive it (idempotency guard sees no live task).
        await ch.send_typing("user@x")
        assert "user@x" in ch._typing_tasks
        await ch.stop_typing("user@x")
        await asyncio.sleep(0.01)


class TestSendDoesNotTouchTyping:
    """send() must no longer manage typing itself — ChannelManager owns that
    lifecycle now (stop_typing on the final outbound). send() touching typing
    again would double-drive it and reintroduce the old coupling."""

    @pytest.mark.asyncio
    async def test_send_does_not_stop_typing(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        ch._send_text = AsyncMock(return_value=wx.SendResult(success=True))
        calls: list[str] = []
        monkeypatch.setattr(ch, "stop_typing", AsyncMock(side_effect=lambda c: calls.append(c)))

        ev = OutboundEvent.text_reply(channel="weixin", chat_id="user@x", text="hi")
        res = await ch.send(ev)

        assert res.success
        assert calls == []  # send() does not drive typing anymore


class TestGetConfigAndSendTypingApi:
    """Exercise the thin _api_post wrappers themselves (not monkeypatched away)."""

    @pytest.mark.asyncio
    async def test_get_config_hits_getconfig_endpoint(self, monkeypatch):
        captured: dict = {}

        async def fake_api_post(session, *, base_url, endpoint, payload, token, timeout_ms):
            captured.update(endpoint=endpoint, payload=payload, token=token, base_url=base_url)
            return {"typing_ticket": "TKT"}

        monkeypatch.setattr(wx, "_api_post", fake_api_post)
        resp = await wx._get_config(
            MagicMock(), base_url="https://api", token="tok", user_id="user@x",
        )
        assert resp == {"typing_ticket": "TKT"}
        assert captured["endpoint"] == wx._EP_GET_CONFIG
        assert captured["payload"] == {"ilink_user_id": "user@x"}
        assert captured["base_url"] == "https://api"
        assert captured["token"] == "tok"

    @pytest.mark.asyncio
    async def test_get_config_includes_context_token(self, monkeypatch):
        captured: dict = {}

        async def fake_api_post(session, *, base_url, endpoint, payload, token, timeout_ms):
            captured.update(payload=payload)
            return {"typing_ticket": "TKT"}

        monkeypatch.setattr(wx, "_api_post", fake_api_post)
        await wx._get_config(
            MagicMock(), base_url="https://api", token="tok",
            user_id="user@x", context_token="ctx-123",
        )
        assert captured["payload"] == {"ilink_user_id": "user@x", "context_token": "ctx-123"}

    @pytest.mark.asyncio
    async def test_send_typing_hits_sendtyping_endpoint(self, monkeypatch):
        captured: dict = {}

        async def fake_api_post(session, *, base_url, endpoint, payload, token, timeout_ms):
            captured.update(endpoint=endpoint, payload=payload)
            return {"errcode": 0}

        monkeypatch.setattr(wx, "_api_post", fake_api_post)
        resp = await wx._send_typing(
            MagicMock(),
            base_url="https://api",
            token="tok",
            to="user@x",
            status=wx._TYPING_START,
            typing_ticket="TKT",
        )
        assert resp == {"errcode": 0}
        assert captured["endpoint"] == wx._EP_SEND_TYPING
        assert captured["payload"] == {
            "ilink_user_id": "user@x",
            "status": wx._TYPING_START,
            "typing_ticket": "TKT",
        }


class TestEnsureTicketEdges:
    @pytest.mark.asyncio
    async def test_none_without_session(self, tmp_path):
        ch = _make_weixin(tmp_path)
        ch._send_session = None
        assert await ch._ensure_typing_ticket("user@x") is None

    @pytest.mark.asyncio
    async def test_cache_hit_skips_getconfig(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        calls = _patch_getconfig(monkeypatch, "FRESH")
        ch._typing_tickets["user@x"] = ("CACHED", time.monotonic())  # within TTL
        assert await ch._ensure_typing_ticket("user@x") == "CACHED"
        assert calls == []  # served from cache, getconfig not called

    @pytest.mark.asyncio
    async def test_expired_ticket_is_refetched(self, tmp_path, monkeypatch):
        """ticket 缓存超过 TTL 后必须经 getconfig 重新拉取,不能复用过期 ticket。

        回归:iLink 服务端 ticket 实际寿命约 600 秒,过期后 sendtyping 被静默拒绝,
        连 status=2 停止都发不出去会导致气泡卡死。之前 TTL 误设为 23h,过期 ticket
        会被长期复用。这里把缓存时间戳回拨到超过 TTL,断言会重拉到新 ticket。
        """
        ch = _make_weixin(tmp_path)
        calls = _patch_getconfig(monkeypatch, "FRESH")
        # 时间戳回拨到 TTL 之前一点,模拟 ticket 已过期
        ch._typing_tickets["user@x"] = ("STALE", time.monotonic() - (wx._TYPING_TICKET_TTL + 1))
        assert await ch._ensure_typing_ticket("user@x") == "FRESH"
        assert len(calls) == 1  # 过期 → 重新 getconfig
        assert ch._typing_tickets["user@x"][0] == "FRESH"  # 缓存被刷新

    @pytest.mark.asyncio
    async def test_ticket_read_from_nested_config(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)

        async def fake_getconfig(session, *, base_url, token, user_id, context_token=None):
            return {"config": {"typing_ticket": "NESTED"}}

        monkeypatch.setattr(wx, "_get_config", fake_getconfig)
        assert await ch._ensure_typing_ticket("user@x") == "NESTED"


class TestTypingLoopExtraBranches:
    @pytest.mark.asyncio
    async def test_loop_exits_on_deadline_with_final_stop(self, tmp_path, monkeypatch):
        """Natural timeout (not cancel): loop ends and still sends a final stop."""
        ch = _make_weixin(tmp_path)
        monkeypatch.setattr(wx, "_TYPING_REFRESH_INTERVAL", 0.0)
        monkeypatch.setattr(wx, "_TYPING_MAX_DURATION", 0.0)  # deadline already passed
        _patch_getconfig(monkeypatch, "TKT")
        sent: list[int] = []

        async def fake_sendtyping(session, *, base_url, token, to, status, typing_ticket):
            sent.append(status)
            return {}

        monkeypatch.setattr(wx, "_send_typing", fake_sendtyping)
        await ch._typing_loop("user@x")
        assert sent == [wx._TYPING_STOP]  # no start (deadline passed), only final stop

    @pytest.mark.asyncio
    async def test_do_send_typing_swallows_send_error(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        _patch_getconfig(monkeypatch, "TKT")

        async def boom(session, **kwargs):
            raise RuntimeError("sendtyping failed")

        monkeypatch.setattr(wx, "_send_typing", boom)
        # Should not raise despite the underlying network error.
        await ch._do_send_typing("user@x", wx._TYPING_START)

    @pytest.mark.asyncio
    async def test_on_typing_done_logs_task_exception(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)

        async def failing_loop(chat_id):
            raise RuntimeError("loop crashed")

        monkeypatch.setattr(ch, "_typing_loop", failing_loop)
        await ch.send_typing("user@x")
        task = ch._typing_tasks["user@x"]
        # Let the task run, crash, and fire the done callback.
        with pytest.raises(RuntimeError):
            await task
        await asyncio.sleep(0)  # allow done callback to run
        assert "user@x" not in ch._typing_tasks
