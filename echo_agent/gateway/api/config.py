from __future__ import annotations

from typing import Any, TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer

# 命中即打码的敏感子串(小写)。按子串而非精确键名匹配,覆盖 admin_tokens、
# credential_pool、authorization、x-api-key 等旁路键;大小写不敏感。
_SENSITIVE_SUBSTRINGS = (
    "api_key", "apikey", "api_token", "token", "secret", "password",
    "credential", "authorization", "auth_header", "private_key", "access_key",
)


def _is_sensitive(key: str) -> bool:
    # 连字符归一为下划线,让 HTTP 头风格的 x-api-key 命中 api_key 等下划线子串,
    # 避免 X-API-Key/Auth-Header 这类旁路键因分隔符差异绕过脱敏。
    k = key.lower().replace("-", "_")
    return any(sub in k for sub in _SENSITIVE_SUBSTRINGS)


def _sanitize(obj: Any, depth: int = 0) -> Any:
    if depth > 10:
        return obj
    if isinstance(obj, dict):
        result = {}
        for k, v in obj.items():
            if isinstance(k, str) and _is_sensitive(k):
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
        guard = self._server._require_admin_token(request, action="config_get")
        if guard is not None:
            return guard

        config = self._get_config()
        if config is None:
            return web.json_response({"error": "config not available"}, status=500)

        data = {}
        for field_name in ("models", "gateway", "session", "memory", "knowledge", "agent", "ui", "evolution"):
            section = getattr(config, field_name, None)
            if section is None:
                continue
            if hasattr(section, "model_dump"):
                # pydantic 模型:mode="json" 递归把子模型/枚举/datetime 转成原生
                # 可 JSON 类型。刻意不带 by_alias —— 保持 snake_case 字段名,既与
                # get_models 端点一致,又让下面按 snake_case 的 _SENSITIVE_KEYS 脱敏
                # 生效(camelCase 别名会绕过脱敏导致密钥泄漏)。
                data[field_name] = section.model_dump(mode="json")
            elif hasattr(section, "to_dict"):
                data[field_name] = section.to_dict()
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
