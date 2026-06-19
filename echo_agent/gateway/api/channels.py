from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class ChannelsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_channels(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "channels_list")
        if guard is not None:
            return guard

        manager = self._server.channel_manager
        active = manager.active_channels

        channel_names = [
            "telegram", "discord", "webhook", "cli", "cron", "slack",
            "whatsapp", "weixin", "qqbot", "feishu", "dingtalk",
            "email", "wecom", "matrix",
        ]

        config = getattr(self._server._agent_loop, "config", None)
        channels_cfg = getattr(config, "channels", None) if config else None

        channels = []
        for name in channel_names:
            ch_cfg = getattr(channels_cfg, name, None) if channels_cfg else None
            if ch_cfg is None:
                continue
            enabled = getattr(ch_cfg, "enabled", False)
            if not enabled:
                continue
            channels.append({
                "name": name,
                "enabled": enabled,
                "running": name in active,
            })

        for name in active:
            if not any(c["name"] == name for c in channels):
                channels.append({"name": name, "enabled": True, "running": True})

        return web.json_response({"channels": channels})
