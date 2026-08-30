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

    def _admin_guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_admin_token(request, action=action)

    async def list_channels(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "channels_list")
        if guard is not None:
            return guard

        manager = self._server.channel_manager
        active = manager.active_channels

        channel_names = [
            "telegram",
            "discord",
            "webhook",
            "cron",
            "slack",
            "whatsapp",
            "weixin",
            "qqbot",
            "feishu",
            "dingtalk",
            "email",
            "wecom",
            "matrix",
        ]

        config = getattr(self._server._agent_loop, "config", None)
        channels_cfg = getattr(config, "channels", None) if config else None

        channels = []
        for name in channel_names:
            ch_cfg = getattr(channels_cfg, name, None) if channels_cfg else None
            if ch_cfg is None:
                continue
            enabled = getattr(ch_cfg, "enabled", False)
            entry: dict = {
                "name": name,
                "enabled": enabled,
                "running": name in active,
            }
            if enabled:
                allow_from = getattr(ch_cfg, "allow_from", None)
                if isinstance(allow_from, (list, tuple)) and allow_from:
                    entry["allow_from_count"] = len(allow_from)
                group_policy = getattr(ch_cfg, "group_policy", None)
                if isinstance(group_policy, str) and group_policy:
                    entry["group_policy"] = group_policy
            channels.append(entry)

        for name in active:
            if name == "cli":
                continue
            if not any(c["name"] == name for c in channels):
                channels.append({"name": name, "enabled": True, "running": True})

        return web.json_response({"channels": channels})

    async def lifecycle(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "channels_lifecycle")
        if guard is not None:
            return guard
        name = request.match_info["name"]
        action = request.match_info["action"]
        if name == "cli":
            return web.json_response(
                {"error": f"channel '{name}' is managed by the process lifecycle"},
                status=409,
            )
        manager = self._server.channel_manager
        if getattr(manager.config, name, None) is None:
            return web.json_response({"error": f"unknown channel '{name}'"}, status=404)
        try:
            if action == "start":
                channel = await manager.start_channel(name)
            elif action == "stop":
                await manager.stop_channel(name)
                channel = manager.get_channel(name)
            elif action == "restart":
                channel = await manager.restart_channel(name)
            else:
                return web.json_response({"error": "unknown lifecycle action"}, status=404)
        except ValueError as exc:
            return web.json_response({"error": str(exc)}, status=409)
        except Exception as exc:
            return web.json_response(
                {"error": f"channel {action} failed: {exc}"},
                status=502,
            )
        return web.json_response(
            {
                "success": True,
                "channel": {
                    "name": name,
                    "enabled": bool(getattr(getattr(manager.config, name), "enabled", False)),
                    "running": bool(channel and channel.is_running),
                },
            }
        )
