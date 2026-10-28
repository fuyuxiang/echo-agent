"""Gateway runtime, streaming, and health regression tests."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

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


# ── Dynamic port binding ─────────────────────────────────────────────────────


class TestDynamicPort:

    @pytest.mark.asyncio
    async def test_actual_port_defaults_to_config(self):
        gw, _, config = _make_gateway(port=8080)
        assert gw.actual_port == 8080

    @pytest.mark.asyncio
    async def test_start_with_port_zero_binds_random(self):
        gw, _, _ = _make_gateway(port=0)
        await gw.start()

        try:
            assert gw.actual_port != 0
            assert gw.actual_port > 1024
        finally:
            await gw.stop()


# ── Removed standalone-client surface stays absent ───────────────────────────


class TestRemovedClientSurface:

    def test_removed_routes_are_not_registered(self):
        from aiohttp import web

        gw, _, config = _make_gateway()
        gw._app = web.Application()
        gw._setup_routes()
        routes = {route.resource.canonical for route in gw._app.router.routes()}

        prefix = config.api_prefix
        assert f"{prefix}/chat/attachments" not in routes
        assert f"{prefix}/config/models" not in routes
        assert f"{prefix}/shutdown" not in routes

    def test_removed_config_contract_is_not_exposed(self):
        from echo_agent.config.schema import GatewayConfig

        assert "desktop" not in GatewayConfig().known_platforms
        assert "emit_progress_events" not in GatewayConfig.model_fields
        assert "progress_debug" not in GatewayConfig.model_fields


# ── Stream channel prefix matching ───────────────────────────────────────────


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


# ── Local channels get the low-latency streaming tier ────────────────────────


class TestStreamFlushParams:
    """本地通道(cli/gateway)用低延迟档位,IM 通道保留段落限流档位。"""

    @staticmethod
    def _loop(**overrides):
        from echo_agent.config.schema import ChannelsConfig
        ch = ChannelsConfig(**overrides)
        loop = MagicMock()
        loop.config.channels = ch
        return loop

    def test_cli_uses_low_latency_tier(self):
        from echo_agent.agent.loop import AgentLoop
        loop = self._loop()
        chars, interval, paragraph = AgentLoop._stream_flush_params(loop, "cli")
        assert (chars, interval, paragraph) == (24, 100, False)

    def test_gateway_wildcard_uses_low_latency_tier(self):
        from echo_agent.agent.loop import AgentLoop
        loop = self._loop()
        assert AgentLoop._stream_flush_params(loop, "gateway:cli") == (24, 100, False)
        assert AgentLoop._stream_flush_params(loop, "gateway:web") == (24, 100, False)

    def test_im_channels_keep_paragraph_tier(self):
        from echo_agent.agent.loop import AgentLoop
        loop = self._loop()
        for channel in ("telegram", "discord", "slack"):
            assert AgentLoop._stream_flush_params(loop, channel) == (180, 1500, True)

    def test_zero_local_chars_falls_back_to_shared_values(self):
        # 显式关闭本地档位后,cli 回落到通用配置(段落模式)。
        from echo_agent.agent.loop import AgentLoop
        loop = self._loop(stream_local_flush_chars=0)
        assert AgentLoop._stream_flush_params(loop, "cli") == (180, 1500, True)


class TestOptimisticStreamChannels:
    """乐观流式(先发草稿后撤回)只允许在能就地重绘的通道上启用。"""

    @staticmethod
    def _stage(**overrides):
        from echo_agent.agent.pipeline.inference_stage import InferenceStage
        from echo_agent.config.schema import ChannelsConfig
        stage = MagicMock()
        stage._config.channels = ChannelsConfig(**overrides)
        return stage, InferenceStage._can_retract_draft

    def test_only_tui_channel_can_retract(self):
        # gateway:cli(TUI,set_markdown 重绘)可撤回;纯打印的 cli 通道不行 ——
        # 它直接写 stdout,撤回会把草稿留在答案上方。
        stage, fn = self._stage()
        assert fn(stage, "gateway:cli") is True
        assert fn(stage, "cli") is False

    def test_send_only_and_im_channels_cannot_retract(self):
        # 默认只放开 TUI;IM 与只发通道一律走保守缓冲。
        stage, fn = self._stage()
        for channel in ("telegram", "discord", "slack", "webhook", "email", "gateway:web"):
            assert fn(stage, channel) is False

    def test_empty_list_disables_optimistic_streaming_everywhere(self):
        stage, fn = self._stage(stream_optimistic_channels=[])
        assert fn(stage, "gateway:cli") is False


# ── Health degradation with StubProvider ─────────────────────────────────────


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
