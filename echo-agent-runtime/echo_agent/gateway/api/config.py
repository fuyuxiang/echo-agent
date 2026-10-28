from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any

from aiohttp import web
import yaml
from pydantic import BaseModel, ValidationError

from echo_agent.config.schema import Config

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer

# schema.py 审计得到的、值为密钥的精确字段名。
_SENSITIVE_EXACT = frozenset(
    {
        "token",
        "bot_token",
        "app_token",
        "verify_token",
        "access_token",
        "verification_token",
        "secret",
        "app_secret",
        "password",
        "encryption_key",
        "encoding_aes_key",
        "app_key",
        "fal_key",
        "api_key",
        "transcription_api_key",
        "search_api_key",
        "openai_api_key",
        "credential_pool",
        "api_tokens",
        "admin_tokens",
        "auth",
    }
)

# 名字看着敏感、实则非密钥的字段:预算数值、头名、env 变量名、作用域键。永不打码。
_SENSITIVE_ALLOWLIST = frozenset(
    {
        "max_tokens",
        "context_window_tokens",
        "summary_min_tokens",
        "summary_max_tokens",
        "token_header",
        "owner_key",
        "encryption_key_env",
        "remote_key_path",
        "remote_strict_host_key",
    }
)

# 动态字典键(如 extra_headers 的 Authorization / X-API-Key)的兜底子串。
_SENSITIVE_SUBSTRINGS = (
    "api_key",
    "apikey",
    "api_token",
    "token",
    "secret",
    "password",
    "credential",
    "authorization",
    "auth_header",
    "private_key",
    "access_key",
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

    def _get_config(self):
        return self._server._agent_loop.config if hasattr(self._server._agent_loop, "config") else None

    @staticmethod
    def _editable_path(path: str) -> bool:
        if not isinstance(path, str):
            return False
        parts = path.split(".")
        if not parts or any(not part or part.startswith("_") for part in parts):
            return False
        if parts[0] not in {
            "ui",
            "observability",
            "session",
            "memory",
            "knowledge",
            "agent",
            "evolution",
            "cost",
            "channels",
        }:
            return False
        return not any(_is_sensitive(part) for part in parts)

    @staticmethod
    def _leaf_changes(changes: dict[str, Any]) -> dict[str, Any]:
        """Expand section patches before validation so containers cannot hide secrets.

        Section updates are merge operations: replacing ``channels.telegram``
        must not erase credentials or reset unrelated settings to defaults.
        """
        leaves: dict[str, Any] = {}

        def visit(path: str, value: Any) -> None:
            if isinstance(value, dict) and value:
                for key, child in value.items():
                    visit(f"{path}.{key}", child)
            else:
                if path in leaves:
                    raise ValueError(f"overlapping configuration path: {path}")
                leaves[path] = value

        for path, value in changes.items():
            if not isinstance(path, str):
                raise ValueError("configuration paths must be strings")
            visit(path, value)
        return leaves

    def _saved_config(self, runtime: Config) -> tuple[Config, dict[str, Any]]:
        from echo_agent.config.loader import _canonicalize_keys

        target = self._config_path()
        try:
            raw = yaml.safe_load(target.read_text(encoding="utf-8")) if target.exists() else {}
        except yaml.YAMLError:
            raise ValueError("saved YAML is invalid") from None
        if raw is None:
            raw = {}
        if not isinstance(raw, dict):
            raise ValueError("root must be a mapping")
        raw = _canonicalize_keys(Config, raw)
        saved = self._resolve_saved(runtime, raw)
        return saved, raw

    @staticmethod
    def _resolve_saved(runtime: Config, raw: dict[str, Any]) -> Config:
        from echo_agent.config.loader import resolve_config_data

        return resolve_config_data(
            raw, overrides=runtime._startup_overrides,
            base=None if runtime._source_loaded else runtime.model_dump(mode="python"),
        )

    @staticmethod
    def _managed_paths(runtime: Config) -> list[str]:
        from echo_agent.config.loader import _deep_merge, _env_overrides

        overrides = _deep_merge(_env_overrides(), runtime._startup_overrides)
        paths = []

        def visit(data: dict, prefix: str = "") -> None:
            for key, value in data.items():
                path = f"{prefix}.{key}" if prefix else key
                if isinstance(value, dict):
                    visit(value, path)
                else:
                    try:
                        ConfigAPI._get_model_path(runtime, path)
                    except (AttributeError, KeyError, TypeError):
                        continue
                    if ConfigAPI._editable_path(path):
                        paths.append(path)

        visit(overrides)
        return sorted(paths)

    @staticmethod
    def _is_managed(path: str, managed: list[str]) -> bool:
        return any(path == item or path.startswith(item + ".") or item.startswith(path + ".") for item in managed)

    @staticmethod
    def _pending_paths(runtime: Any, saved: Any, prefix: str = "") -> list[str]:
        if isinstance(runtime, dict) and isinstance(saved, dict):
            return [
                path
                for key in sorted(runtime.keys() | saved.keys())
                for path in ConfigAPI._pending_paths(
                    runtime.get(key), saved.get(key), f"{prefix}.{key}" if prefix else key
                )
            ]
        return [prefix] if runtime != saved else []

    @staticmethod
    def _set_mapping_path(data: dict[str, Any], path: str, value: Any) -> None:
        parts = path.split(".")
        cursor = data
        for part in parts[:-1]:
            child = cursor.get(part)
            if not isinstance(child, dict):
                child = {}
                cursor[part] = child
            cursor = child
        cursor[parts[-1]] = value

    @staticmethod
    def _get_model_path(model: Any, path: str) -> Any:
        value = model
        for part in path.split("."):
            value = value[part] if isinstance(value, dict) else getattr(value, part)
        return value

    @staticmethod
    def _serializable_value(value: Any) -> Any:
        """Convert a validated nested value to YAML-safe JSON primitives."""
        if hasattr(value, "model_dump"):
            return value.model_dump(mode="json")
        if hasattr(value, "value"):
            return value.value
        if isinstance(value, Path):
            return str(value)
        if isinstance(value, dict):
            return {str(k): ConfigAPI._serializable_value(v) for k, v in value.items()}
        if isinstance(value, (list, tuple, set, frozenset)):
            return [ConfigAPI._serializable_value(item) for item in value]
        return value

    def _config_path(self) -> Path:
        path = getattr(self._server, "_config_path", None)
        if path is not None:
            return Path(path)
        return Path(self._server._workspace) / "echo-agent.yaml"

    async def get_config(self, request: web.Request) -> web.Response:
        guard = self._server._require_admin_token(request, action="config_get")
        if guard is not None:
            return guard

        config = self._get_config()
        if config is None:
            return web.json_response({"error": "config not available"}, status=500)

        runtime = config
        config_error = None
        if isinstance(runtime, Config):
            try:
                config, _ = self._saved_config(runtime)
            except (OSError, ValueError, yaml.YAMLError) as exc:
                # Never expose validation input values (which may be secrets).
                config_error = f"Saved configuration cannot be loaded ({type(exc).__name__})"

        data = {}
        running = {}
        for field_name in (
            "models",
            "gateway",
            "session",
            "memory",
            "knowledge",
            "agent",
            "ui",
            "evolution",
            "observability",
            "cost",
            "channels",
        ):
            section = getattr(config, field_name, None)
            if section is None:
                continue
            if hasattr(section, "model_dump"):
                # pydantic 模型:mode="json" 递归把子模型/枚举/datetime 转成原生
                # 可 JSON 类型。刻意不带 by_alias —— 保持 snake_case 字段名，确保
                # 下面按 snake_case 的 _SENSITIVE_KEYS 脱敏生效（camelCase 别名会
                # 绕过脱敏导致密钥泄漏）。
                dumped = section.model_dump(mode="json")
                data[field_name] = dumped if isinstance(dumped, dict) else str(section)
            elif hasattr(section, "to_dict"):
                dumped = section.to_dict()
                data[field_name] = dumped if isinstance(dumped, dict) else str(section)
            else:
                data[field_name] = str(section)
            active = getattr(runtime, field_name, None)
            if isinstance(runtime, Config) and hasattr(active, "model_dump"):
                running[field_name] = active.model_dump(mode="json")

        response = _sanitize(data)
        pending = self._pending_paths(running, data) if isinstance(runtime, Config) else []
        response["_meta"] = {
            "editable_roots": [
                "ui",
                "observability",
                "session",
                "memory",
                "knowledge",
                "agent",
                "evolution",
                "cost",
                "channels",
            ],
            "config_path": str(self._config_path()),
            "restart_required": bool(pending),
            "pending_paths": pending,
            "running": _sanitize(running),
            "config_error": config_error,
            "managed_paths": self._managed_paths(runtime) if isinstance(runtime, Config) else [],
        }
        return web.json_response(response)

    async def update_config(self, request: web.Request) -> web.Response:
        guard = self._server._require_admin_token(request, action="config_update")
        if guard is not None:
            return guard
        config = self._get_config()
        if config is None:
            return web.json_response({"error": "config not available"}, status=500)
        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)
        changes = body.get("changes") if isinstance(body, dict) else None
        if not isinstance(changes, dict) or not changes:
            return web.json_response(
                {"error": "a non-empty 'changes' object is required"},
                status=400,
            )
        try:
            changes = self._leaf_changes(changes)
        except ValueError as exc:
            return web.json_response({"error": str(exc)}, status=400)
        invalid = [path for path in changes if not self._editable_path(path)]
        for path in changes:
            try:
                current = self._get_model_path(config, path)
                if isinstance(current, BaseModel) and changes[path] == {}:
                    invalid.append(path)
            except (AttributeError, KeyError, TypeError):
                invalid.append(path)
        if invalid:
            return web.json_response(
                {"error": "field is read-only or sensitive", "paths": invalid},
                status=400,
            )

        managed = self._managed_paths(config)
        locked = [path for path in changes if self._is_managed(path, managed)]
        if locked:
            return web.json_response(
                {"error": "settings are managed by environment variables or startup arguments; update the service settings and restart",
                 "code": "config_managed", "paths": locked}, status=409,
            )

        try:
            saved, raw = self._saved_config(config)
        except (OSError, ValueError, yaml.YAMLError) as exc:
            return web.json_response(
                {"error": f"cannot update invalid config file ({type(exc).__name__})"}, status=409
            )
        effective = saved.model_dump(mode="python")
        for path, value in changes.items():
            self._set_mapping_path(effective, path, value)
        try:
            validated = Config.model_validate(effective)
        except ValidationError as exc:
            errors = [{"path": ".".join(str(p) for p in item["loc"]), "message": item["msg"]} for item in exc.errors()]
            return web.json_response({"error": "validation failed", "details": errors}, status=400)

        target = self._config_path()
        for path in changes:
            value = self._get_model_path(validated, path)
            self._set_mapping_path(raw, path, self._serializable_value(value))

        # Resolve the exact saved payload with startup precedence too, so the
        # response and later channel activation describe the same next runtime.
        validated = self._resolve_saved(config, raw)

        from echo_agent.config.loader import save_config

        try:
            save_config(raw, target)
        except OSError as exc:
            return web.json_response({"error": f"save failed: {exc}"}, status=500)

        # Runtime objects capture settings during construction. Keep that model
        # truthful until restart; explicit channel lifecycle actions remain live.
        pending = self._pending_paths(config.model_dump(mode="json"), validated.model_dump(mode="json"))
        await self._server.dashboard_ws.broadcast(
            "config_updated",
            {"paths": list(changes), "restart_required": bool(pending)},
        )
        return web.json_response(
            {
                "success": True,
                "paths": list(changes),
                "restart_required": bool(pending),
                "pending_paths": pending,
                "config_path": str(target),
            }
        )
