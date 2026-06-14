"""Tests for desktop collaboration features:
- Dynamic port + ready signal (Requirement 3)
- Lifecycle API / shutdown route (Requirement 4)
- Gateway streaming channel matching with prefix (Requirement 1 / P2 fix)
- Progress events emit_progress_events toggle (Requirement 2)
- Health degraded with StubProvider (Requirement 5)
"""

from __future__ import annotations

import asyncio
import io
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.bus.queue import MessageBus


def _make_gateway(port=19999, agent_loop=None):
    from echo_agent.gateway.server import GatewayServer
    from echo_agent.config.schema import GatewayConfig, GatewayAuthConfig, GatewaySessionPolicyConfig

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=port,
        auth=GatewayAuthConfig(mode="open"),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    bus = MessageBus()
    channel_manager = MagicMock()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    workspace = MagicMock()
    loop = agent_loop or MagicMock()

    gw = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=channel_manager,
        session_manager=session_manager,
        workspace=workspace,
        agent_loop=loop,
    )
    return gw, bus, config


# ── Requirement 3: Dynamic port + ready signal ───────────────────────────────


class TestDynamicPortAndReadySignal:

    @pytest.mark.asyncio
    async def test_actual_port_defaults_to_config(self):
        gw, _, config = _make_gateway(port=8080)
        assert gw.actual_port == 8080

    @pytest.mark.asyncio
    async def test_start_prints_ready_signal(self):
        gw, _, _ = _make_gateway(port=0)
        captured = io.StringIO()

        with patch("sys.stdout", captured):
            await gw.start()

        try:
            output = captured.getvalue()
            assert "ECHO_AGENT_READY port=" in output
            assert "ws=" in output
            assert "health=" in output

            port_str = output.split("port=")[1].split(" ")[0]
            actual = int(port_str)
            assert actual > 0
            assert gw.actual_port == actual
        finally:
            await gw.stop()

    @pytest.mark.asyncio
    async def test_start_with_port_zero_binds_random(self):
        gw, _, _ = _make_gateway(port=0)
        captured = io.StringIO()

        with patch("sys.stdout", captured):
            await gw.start()

        try:
            assert gw.actual_port != 0
            assert gw.actual_port > 1024
        finally:
            await gw.stop()


# ── Requirement 4: Lifecycle API / shutdown ──────────────────────────────────


class TestLifecycleAPI:

    @pytest.mark.asyncio
    async def test_shutdown_with_event_returns_202(self):
        from echo_agent.gateway.api.lifecycle import LifecycleAPI

        gw, _, _ = _make_gateway()
        shutdown_event = asyncio.Event()
        gw.set_shutdown_event(shutdown_event)

        api = LifecycleAPI(gw)
        request = MagicMock()
        request.headers = {}

        response = await api.shutdown(request)
        assert response.status == 202
        assert shutdown_event.is_set()

    @pytest.mark.asyncio
    async def test_shutdown_without_event_returns_503(self):
        from echo_agent.gateway.api.lifecycle import LifecycleAPI

        gw, _, _ = _make_gateway()

        api = LifecycleAPI(gw)
        request = MagicMock()
        request.headers = {}

        response = await api.shutdown(request)
        assert response.status == 503

    def test_request_shutdown_noop_without_event(self):
        gw, _, _ = _make_gateway()
        gw.request_shutdown()

    def test_request_shutdown_sets_event(self):
        gw, _, _ = _make_gateway()
        event = asyncio.Event()
        gw.set_shutdown_event(event)
        gw.request_shutdown()
        assert event.is_set()


# ── Requirement 1 + P2: Stream channel prefix matching ───────────────────────


class TestStreamChannelMatching:

    def test_exact_match(self):
        from echo_agent.agent.loop import AgentLoop
        config = MagicMock()
        config.channels.stream_channels = ["cli", "telegram", "gateway:ws"]

        loop = MagicMock()
        loop.config = config
        assert AgentLoop._should_stream_channel(loop, "cli") is True

    def test_prefix_wildcard_match(self):
        from echo_agent.agent.loop import AgentLoop
        config = MagicMock()
        config.channels.stream_channels = ["cli", "gateway:*"]

        loop = MagicMock()
        loop.config = config

        assert AgentLoop._should_stream_channel(loop, "gateway:ws") is True
        assert AgentLoop._should_stream_channel(loop, "gateway:web") is True
        assert AgentLoop._should_stream_channel(loop, "gateway:api") is True

    def test_no_match(self):
        from echo_agent.agent.loop import AgentLoop
        config = MagicMock()
        config.channels.stream_channels = ["cli", "gateway:*"]

        loop = MagicMock()
        loop.config = config

        assert AgentLoop._should_stream_channel(loop, "telegram") is False
        assert AgentLoop._should_stream_channel(loop, "discord") is False

    def test_gateway_default_config_includes_wildcard(self):
        from echo_agent.config.schema import ChannelsConfig
        config = ChannelsConfig()
        assert "gateway:*" in config.stream_channels


# ── Requirement 5: Health degraded with StubProvider ─────────────────────────


class TestHealthDegraded:

    @pytest.mark.asyncio
    async def test_healthy_with_normal_provider(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = MagicMock()
        gw.is_running = True
        gw.channel_manager = MagicMock()
        gw.channel_manager.active_channels = ["telegram"]
        gw.rate_limiter = None
        gw.media_cache = MagicMock()
        gw.media_cache.get_size_mb.return_value = 0
        gw.session_manager = MagicMock(spec=["list_sessions"])
        gw.session_manager.list_sessions.return_value = []
        gw.hooks = MagicMock()
        gw.hooks.handler_count = 0
        gw.delivery_router = MagicMock()
        gw.delivery_router.rule_count = 0

        agent_loop = MagicMock()
        agent_loop.provider = MagicMock()
        agent_loop.provider.is_stub = False
        gw._agent_loop = agent_loop

        provider = GatewayHealthProvider(gw)
        result = await provider.check()
        assert result["status"] == "healthy"
        assert result["provider"] == "ok"

    @pytest.mark.asyncio
    async def test_degraded_with_stub_provider(self):
        from echo_agent.gateway.health import GatewayHealthProvider

        gw = MagicMock()
        gw.is_running = True
        gw.channel_manager = MagicMock()
        gw.channel_manager.active_channels = ["telegram"]
        gw.rate_limiter = None
        gw.media_cache = MagicMock()
        gw.media_cache.get_size_mb.return_value = 0
        gw.session_manager = MagicMock(spec=["list_sessions"])
        gw.session_manager.list_sessions.return_value = []
        gw.hooks = MagicMock()
        gw.hooks.handler_count = 0
        gw.delivery_router = MagicMock()
        gw.delivery_router.rule_count = 0

        agent_loop = MagicMock()
        agent_loop.provider = MagicMock()
        agent_loop.provider.is_stub = True
        gw._agent_loop = agent_loop

        provider = GatewayHealthProvider(gw)
        result = await provider.check()
        assert result["status"] == "degraded"
        assert result["provider"] == "stub"


# ── Requirement 2: Progress events config toggle ─────────────────────────────


class TestProgressEventsToggle:

    def test_emit_progress_events_default_true(self):
        from echo_agent.config.schema import GatewayConfig
        config = GatewayConfig()
        assert config.emit_progress_events is True

    def test_progress_debug_default_false(self):
        from echo_agent.config.schema import GatewayConfig
        config = GatewayConfig()
        assert config.progress_debug is False