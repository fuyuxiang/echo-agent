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
        from echo_agent.config.schema import Config
        from echo_agent.gateway.api.config import ConfigAPI

        saved_channels = channels_cfg
        managed_paths = ConfigAPI._managed_paths(config) if isinstance(config, Config) else []
        if isinstance(config, Config):
            try:
                saved, _ = ConfigAPI(self._server)._saved_config(config)
                saved_channels = saved.channels
            except (OSError, ValueError):
                # Config management reports invalid saved files explicitly;
                # operational channel status remains available in that case.
                pass

        channels = []
        for name in channel_names:
            ch_cfg = getattr(channels_cfg, name, None) if channels_cfg else None
            if ch_cfg is None:
                continue
            saved_cfg = getattr(saved_channels, name, ch_cfg)
            enabled = getattr(saved_cfg, "enabled", False)
            entry: dict = {
                "name": name,
                "enabled": enabled,
                "running": name in active,
                "runtime_enabled": bool(getattr(ch_cfg, "enabled", False)),
                "restart_required": saved_cfg != ch_cfg,
                "enabled_managed": ConfigAPI._is_managed(f"channels.{name}.enabled", managed_paths),
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
        if action not in {"start", "stop", "restart"}:
            return web.json_response({"error": "unknown lifecycle action"}, status=404)
        if name == "cli":
            return web.json_response(
                {"error": f"channel '{name}' is managed by the process lifecycle"},
                status=409,
            )
        manager = self._server.channel_manager
        if getattr(manager.config, name, None) is None:
            return web.json_response({"error": f"unknown channel '{name}'"}, status=404)
        try:
            # Start/restart/stop is the explicit live activation boundary. A
            # generic config save must not silently reconfigure other runtime
            # modules, but the channel page promises immediate lifecycle control.
            from echo_agent.config.schema import Config
            from echo_agent.gateway.api.config import ConfigAPI

            config = getattr(self._server._agent_loop, "config", None)
            if isinstance(config, Config) and action != "stop":
                try:
                    saved, _ = ConfigAPI(self._server)._saved_config(config)
                except (OSError, ValueError):
                    return web.json_response(
                        {"error": "saved configuration is invalid; correct it before changing channel lifecycle"}, status=409,
                    )
                updated = getattr(saved.channels, name)
                existing = manager.get_channel(name)
                if (action == "start" and existing is not None and existing.is_running
                        and getattr(manager.config, name) != updated):
                    return web.json_response(
                        {"error": "channel is running with different settings; restart it to apply the saved configuration"},
                        status=409,
                    )
                setattr(manager.config, name, updated)
            if action == "start":
                channel = await manager.start_channel(name)
            elif action == "stop":
                await manager.stop_channel(name)
                channel = manager.get_channel(name)
                # Stopping never depends on valid replacement settings. When
                # available, update only enabled to keep runtime status useful;
                # credentials/allowlists remain those of the stopped instance.
                if isinstance(config, Config):
                    try:
                        saved, _ = ConfigAPI(self._server)._saved_config(config)
                    except (OSError, ValueError):
                        # Stop already succeeded; invalid replacement settings
                        # must not turn that recovery action into a failure.
                        pass
                    else:
                        getattr(manager.config, name).enabled = getattr(saved.channels, name).enabled
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
