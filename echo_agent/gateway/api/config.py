from __future__ import annotations

from typing import Any, TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer

# schema.py 审计得到的、值为密钥的精确字段名。
_SENSITIVE_EXACT = frozenset({
    "token", "bot_token", "app_token", "verify_token", "access_token",
    "verification_token", "secret", "app_secret", "password",
    "encryption_key", "encoding_aes_key", "app_key", "fal_key",
    "api_key", "transcription_api_key", "search_api_key", "openai_api_key",
    "credential_pool", "api_tokens", "admin_tokens", "auth",
})

# 名字看着敏感、实则非密钥的字段:预算数值、头名、env 变量名、作用域键。永不打码。
_SENSITIVE_ALLOWLIST = frozenset({
    "max_tokens", "context_window_tokens", "summary_min_tokens",
    "summary_max_tokens", "token_header", "owner_key",
    "encryption_key_env", "remote_key_path", "remote_strict_host_key",
})

# 动态字典键(如 extra_headers 的 Authorization / X-API-Key)的兜底子串。
_SENSITIVE_SUBSTRINGS = (
    "api_key", "apikey", "api_token", "token", "secret", "password",
    "credential", "authorization", "auth_header", "private_key", "access_key",
)


def _is_sensitive(key: str) -> bool:
    if not isinstance(key, str):
        return False
    k = key.lower()
    if k in _SENSITIVE_ALLOWLIST:
        return False
    if k in _SENSITIVE_EXACT:
        return True
    # 连字符归一为下划线,使 x-api-key 命中 api_key;兜底动态头名。
    kn = k.replace("-", "_")
    return any(sub in kn for sub in _SENSITIVE_SUBSTRINGS)


def _sanitize(obj: Any, depth: int = 0) -> Any:
    if depth > 10:
        return obj
    if isinstance(obj, dict):
        result = {}
        for k, v in obj.items():
            if isinstance(k, str) and _is_sensitive(k) and not isinstance(v, dict):
                # 敏感键的叶子/列表值整体打码;若值为 dict(如 gateway.auth 容器),
                # 递归进入,让其子键(admin_tokens 等)逐个按各自敏感性处理。
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
