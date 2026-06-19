from __future__ import annotations

from typing import Any, TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer

_SENSITIVE_KEYS = frozenset({
    "api_key", "api_keys", "api_tokens", "secret", "password",
    "token", "access_token", "secret_key", "private_key",
})


def _sanitize(obj: Any, depth: int = 0) -> Any:
    if depth > 10:
        return obj
    if isinstance(obj, dict):
        result = {}
        for k, v in obj.items():
            if k in _SENSITIVE_KEYS:
                result[k] = "***" if v else ""
            else:
                result[k] = _sanitize(v, depth + 1)
        return result
    if isinstance(obj, list):
        return [_sanitize(item, depth + 1) for item in obj]
    return obj


class ConfigAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _get_config(self):
        return self._server._agent_loop.config if hasattr(self._server._agent_loop, "config") else None

    async def get_config(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "config_get")
        if guard is not None:
            return guard

        config = self._get_config()
        if config is None:
            return web.json_response({"error": "config not available"}, status=500)

        data = {}
        for field_name in ("models", "gateway", "session", "memory", "knowledge", "agent", "ui", "evolution"):
            section = getattr(config, field_name, None)
            if section is not None:
                if hasattr(section, "to_dict"):
                    data[field_name] = section.to_dict()
                elif hasattr(section, "__dict__"):
                    data[field_name] = vars(section)
                else:
                    data[field_name] = str(section)

        return web.json_response(_sanitize(data))

    async def get_models(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "config_models")
        if guard is not None:
            return guard

        config = self._get_config()
        if config is None:
            return web.json_response({"error": "config not available"}, status=500)

        models_cfg = config.models
        providers = []
        for pc in models_cfg.providers:
            providers.append({
                "name": pc.name,
                "type": pc.type if hasattr(pc, "type") else "",
                "default_model": pc.default_model if hasattr(pc, "default_model") else "",
            })

        return web.json_response({
            "default_model": models_cfg.default_model,
            "providers": providers,
        })
