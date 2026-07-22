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

        # "cli" is intentionally omitted: it is not a standing delivery channel.
        # The in-process CLIChannel only runs when the gateway owns an
        # interactive tty (sys.stdin.isatty()), so under the normal
        # daemon-gateway + attach deployment it can never be `running` and would
        # sit here permanently "offline", which is misleading. Interactive CLI
        # sessions attach over the /ws socket instead and are surfaced by the
        # health endpoint's ws_clients count, not the channel list.
        channel_names = [
            "telegram", "discord", "webhook", "cron", "slack",
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
            if name == "cli":
                continue
            if not any(c["name"] == name for c in channels):
                channels.append({"name": name, "enabled": True, "running": True})

        return web.json_response({"channels": channels})
