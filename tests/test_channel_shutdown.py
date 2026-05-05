"""Tests for channel shutdown timeout and heartbeat ACK detection."""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest


class TestWeixinShutdownTimeout:
    """Tests for WeChat channel graceful shutdown with timeout."""

    @pytest.mark.asyncio
    async def test_stop_completes_within_timeout(self) -> None:
        from echo_agent.channels.weixin import WeixinChannel

        config = MagicMock()
        config.account_id = "test_acc"
        config.token = "test_token"
        config.base_url = "https://example.com"
        config.data_dir = "/tmp/test_weixin"
        config.dm_policy = MagicMock()
        config.dm_policy.mode = "open"
        config.dm_policy.allowlist = []
        bus = MagicMock()

        ch = WeixinChannel(config, bus)
        ch._running = True
        ch._poll_session = MagicMock()
        ch._poll_session.closed = False
        ch._poll_session.close = AsyncMock()
        ch._send_session = MagicMock()
        ch._send_session.closed = False
        ch._send_session.close = AsyncMock()

        async def stuck_poll():
            await asyncio.sleep(100)

        ch._poll_task = asyncio.create_task(stuck_poll())

        await asyncio.wait_for(ch.stop(), timeout=10)
        assert ch._poll_task is None
        assert ch._running is False

    @pytest.mark.asyncio
    async def test_stop_without_poll_task(self) -> None:
        from echo_agent.channels.weixin import WeixinChannel

        config = MagicMock()
        config.account_id = "test_acc"
        config.token = "test_token"
        config.base_url = "https://example.com"
        config.data_dir = "/tmp/test_weixin"
        config.dm_policy = MagicMock()
        config.dm_policy.mode = "open"
        config.dm_policy.allowlist = []
        bus = MagicMock()

        ch = WeixinChannel(config, bus)
        ch._running = True
        ch._poll_task = None
        ch._poll_session = None
        ch._send_session = None

        await ch.stop()
        assert ch._running is False


class TestQQBotHeartbeatACK:
    """Tests for QQBot heartbeat ACK timeout detection."""

    @pytest.mark.asyncio
    async def test_heartbeat_ack_received_resets_flag(self) -> None:
        from echo_agent.channels.qqbot import QQBotChannel

        config = MagicMock()
        config.app_id = "test"
        config.client_secret = "secret"
        config.sandbox = False
        config.dm_policy = MagicMock()
        config.dm_policy.mode = "open"
        config.dm_policy.allowlist = []
        config.media_enabled = False
        config.media_parse_tags = False
        config.media_max_file_size_mb = 10
        config.media_upload_cache_size = 100
        bus = MagicMock()

        ch = QQBotChannel(config, bus)
        ch._heartbeat_ack_received = False

        await ch._handle_ws_message({"op": 11})
        assert ch._heartbeat_ack_received is True

    @pytest.mark.asyncio
    async def test_heartbeat_loop_closes_ws_on_missing_ack(self) -> None:
        from echo_agent.channels.qqbot import QQBotChannel

        config = MagicMock()
        config.app_id = "test"
        config.client_secret = "secret"
        config.sandbox = False
        config.dm_policy = MagicMock()
        config.dm_policy.mode = "open"
        config.dm_policy.allowlist = []
        config.media_enabled = False
        config.media_parse_tags = False
        config.media_max_file_size_mb = 10
        config.media_upload_cache_size = 100
        bus = MagicMock()

        ch = QQBotChannel(config, bus)
        ch._running = True
        ch._heartbeat_ack_received = False
        ch._heartbeat_interval = 0.01

        mock_ws = MagicMock()
        mock_ws.closed = False
        mock_ws.close = AsyncMock()
        ch._ws = mock_ws

        await ch._heartbeat_loop()
        mock_ws.close.assert_called_once()


class TestDiscordHeartbeatACK:
    """Tests for Discord heartbeat ACK timeout detection."""

    @pytest.mark.asyncio
    async def test_heartbeat_ack_sets_flag(self) -> None:
        from echo_agent.channels.discord import DiscordChannel

        config = MagicMock()
        config.token = "test_token"
        config.dm_policy = MagicMock()
        config.dm_policy.mode = "open"
        config.dm_policy.allowlist = []
        bus = MagicMock()

        ch = DiscordChannel(config, bus)
        ch._heartbeat_ack_received = False

        await ch._handle_ws_message({"op": 11})
        assert ch._heartbeat_ack_received is True

    @pytest.mark.asyncio
    async def test_heartbeat_loop_reconnects_on_missing_ack(self) -> None:
        from echo_agent.channels.discord import DiscordChannel

        config = MagicMock()
        config.token = "test_token"
        config.dm_policy = MagicMock()
        config.dm_policy.mode = "open"
        config.dm_policy.allowlist = []
        bus = MagicMock()

        ch = DiscordChannel(config, bus)
        ch._running = True
        ch._heartbeat_ack_received = False
        ch._heartbeat_interval = 0.01

        mock_ws = MagicMock()
        mock_ws.closed = False
        mock_ws.close = AsyncMock()
        ch._ws = mock_ws

        await ch._heartbeat_loop()
        mock_ws.close.assert_called_once()


class TestChannelManagerStopTimeout:
    """Tests for ChannelManager.stop_all timeout handling."""

    @pytest.mark.asyncio
    async def test_stop_all_handles_slow_channel(self) -> None:
        from echo_agent.channels.manager import ChannelManager

        slow_channel = MagicMock()

        async def slow_stop():
            await asyncio.sleep(100)

        slow_channel.stop = slow_stop
        slow_channel.name = "slow"

        manager = ChannelManager.__new__(ChannelManager)
        manager._channels = {"slow": slow_channel}

        await asyncio.wait_for(manager.stop_all(), timeout=15)

    @pytest.mark.asyncio
    async def test_stop_all_continues_after_one_fails(self) -> None:
        from echo_agent.channels.manager import ChannelManager

        ch1 = MagicMock()
        ch1.stop = AsyncMock(side_effect=RuntimeError("oops"))
        ch1.name = "ch1"

        ch2 = MagicMock()
        ch2.stop = AsyncMock()
        ch2.name = "ch2"

        manager = ChannelManager.__new__(ChannelManager)
        manager._channels = {"ch1": ch1, "ch2": ch2}

        await manager.stop_all()
        ch2.stop.assert_called_once()
