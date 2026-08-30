"""Configuration loader — reads YAML, env vars, and CLI overrides."""

from __future__ import annotations

import json
import os
import tempfile
import types
import typing
from pathlib import Path
from typing import Any, get_args, get_origin

import yaml
from loguru import logger
from pydantic import BaseModel

from echo_agent.config.schema import Config
from echo_agent.runtime_paths import default_config_path, echo_home

_UNION_ORIGINS = (typing.Union, types.UnionType)
_DEFAULT_CONFIG_NAMES = ("echo-agent.yaml", "echo-agent.yml", "config.yaml", "config.yml")
_PACKAGED_DEFAULT_CONFIG = Path(__file__).with_name("default.yaml")


def _find_config_file_in(base: Path) -> Path | None:
    for name in _DEFAULT_CONFIG_NAMES:
        candidate = base / name
        if candidate.exists():
            return candidate
    return None


def _candidate_config_dirs(search_dir: Path | None = None, include_home: bool = True) -> list[Path]:
    dirs: list[Path] = []
    if search_dir is not None:
        dirs.append(search_dir.expanduser())
    else:
        dirs.append(Path.cwd())
    if include_home:
        home_dir = echo_home()
        if not any(existing.expanduser() == home_dir for existing in dirs):
            dirs.append(home_dir)
    return dirs


def _find_config_file(search_dir: Path | None = None, include_home: bool = True) -> Path | None:
    for base in _candidate_config_dirs(search_dir, include_home=include_home):
        found = _find_config_file_in(base)
        if found:
            return found
    return None


def find_local_config_file(search_dir: str | Path | None = None) -> Path | None:
    if search_dir is None:
        return _find_config_file(Path.cwd(), include_home=False)
    return _find_config_file(Path(search_dir), include_home=False)


def resolve_config_file(config_path: str | Path | None = None, search_dir: str | Path | None = None) -> Path | None:
    if config_path:
        path = Path(config_path).expanduser()
        return path.resolve() if path.exists() else path
    base = Path(search_dir).expanduser() if search_dir else None
    found = _find_config_file(base, include_home=True)
    return found.resolve() if found else None


class ConfigError(Exception):
    """User-facing configuration error with a readable message."""


def _load_yaml_file(path: Path | None) -> dict[str, Any]:
    if not path or not path.exists():
        return {}
    with open(path, encoding="utf-8") as f:
        try:
            return yaml.safe_load(f) or {}
        except yaml.YAMLError as e:
            mark = getattr(e, "problem_mark", None)
            location = f" (line {mark.line + 1}, column {mark.column + 1})" if mark else ""
            raise ConfigError(f"Invalid YAML in {path}{location}: {getattr(e, 'problem', e)}") from e


def _deep_merge(base: dict, override: dict) -> dict:
    merged = dict(base)
    for key, value in override.items():
        if key in merged and isinstance(merged[key], dict) and isinstance(value, dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _unwrap_optional(annotation: Any) -> Any:
    """Reduce ``X | None`` to ``X``; leave genuine multi-type unions alone."""
    if get_origin(annotation) in _UNION_ORIGINS:
        args = [a for a in get_args(annotation) if a is not type(None)]
        if len(args) == 1:
            return args[0]
    return annotation


def _resolve_annotation(model: type[BaseModel], parts: list[str]) -> Any:
    """Walk a lowercased env path against the schema, returning the declared
    annotation of the addressed field, or None when the path does not resolve.

    Env var names lose their case and their word boundaries collide with the
    ``__`` separator, so a path is only trusted when every segment matches a
    real field name. Anything ambiguous yields None and the value stays a
    string — the conservative direction, since that is the long-standing
    behaviour for every scalar setting.
    """
    if not parts:
        return None
    field = model.model_fields.get(parts[0])
    if field is None:
        return None
    annotation = _unwrap_optional(field.annotation)
    rest = parts[1:]
    if not rest:
        return annotation
    if isinstance(annotation, type) and issubclass(annotation, BaseModel):
        return _resolve_annotation(annotation, rest)
    # A mapping of sub-models (mcp servers, gateway platforms) is addressed as
    # ``...__<user_key>__<field>``, so the next segment is a user-chosen key
    # rather than a field name. A key containing ``__`` is unresolvable and
    # correctly falls through to None.
    if get_origin(annotation) is dict:
        args = get_args(annotation)
        value_type = _unwrap_optional(args[1]) if len(args) > 1 else None
        if isinstance(value_type, type) and issubclass(value_type, BaseModel) and len(rest) > 1:
            return _resolve_annotation(value_type, rest[1:])
    return None


def _canonicalize_keys(model: type[BaseModel], data: Any) -> Any:
    """Rewrite camelCase aliases to their snake_case field names.

    The schema accepts both spellings (``populate_by_name``), so ``apiPrefix``
    and ``api_prefix`` are the same setting — but they are *different dict keys*,
    so a merge keeps both and pydantic silently prefers the alias. That made
    every ``ECHO_AGENT_*`` override a no-op against the camelCase keys used by
    the packaged default.yaml and the setup wizard. Normalizing each source to
    field names before merging collapses the two spellings onto one key so the
    documented precedence (env beats YAML) actually holds.

    Unknown keys are preserved untouched: pydantic ignores extras, and the
    compat migrations run on raw data that must survive this pass.
    """
    if not isinstance(data, dict):
        return data
    by_alias = {f.alias: name for name, f in model.model_fields.items() if f.alias and f.alias != name}
    result: dict[str, Any] = {}
    for key, value in data.items():
        name = key if key in model.model_fields else by_alias.get(key, key)
        field = model.model_fields.get(name)
        if field is not None:
            annotation = _unwrap_optional(field.annotation)
            if isinstance(annotation, type) and issubclass(annotation, BaseModel):
                value = _canonicalize_keys(annotation, value)
            else:
                value = _canonicalize_nested(annotation, value)
        # A later alias must not clobber an explicit field-name key already set
        # by this same source; keep the first spelling encountered.
        if name in result and key != name:
            continue
        result[name] = value
    return result


def _canonicalize_nested(annotation: Any, value: Any) -> Any:
    """Recurse into list/dict containers whose members are sub-models."""
    origin = get_origin(annotation)
    args = get_args(annotation)
    if origin is dict and len(args) > 1:
        member = _unwrap_optional(args[1])
        if isinstance(member, type) and issubclass(member, BaseModel) and isinstance(value, dict):
            return {k: _canonicalize_keys(member, v) for k, v in value.items()}
    elif origin is list and args:
        member = _unwrap_optional(args[0])
        if isinstance(member, type) and issubclass(member, BaseModel) and isinstance(value, list):
            return [_canonicalize_keys(member, v) for v in value]
    return value


def _coerce_env_value(parts: list[str], value: str) -> Any:
    """Parse ``value`` as JSON only when the schema wants a container there."""
    annotation = _resolve_annotation(Config, list(parts))
    if annotation is None:
        return value
    if get_origin(annotation) not in (list, dict) and annotation not in (list, dict):
        return value
    try:
        return json.loads(value)
    except json.JSONDecodeError:
        # Keep the raw string so pydantic reports the offending field by name
        # instead of the loader swallowing a malformed value.
        return value


def _env_overrides() -> dict[str, Any]:
    """Collect ECHO_AGENT_ prefixed env vars into a nested dict.

    Values stay strings unless the *schema* declares the target field as a
    container (list/dict), in which case the string is parsed as JSON. Keying
    the decision off the declared type — rather than off what the value looks
    like — is what keeps a secret that happens to read ``false`` or ``null``
    from turning into a bool/None and failing validation on a ``str`` field.
    """
    prefix = "ECHO_AGENT_"
    result: dict[str, Any] = {}
    for key, value in os.environ.items():
        if not key.startswith(prefix):
            continue
        parts = key[len(prefix) :].lower().split("__")
        parsed_value = _coerce_env_value(parts, value)
        current = result
        for part in parts[:-1]:
            current = current.setdefault(part, {})
        current[parts[-1]] = parsed_value
    return result


def profile_explicitly_set(config_path: str | Path | None = None) -> bool:
    """Whether the user explicitly set ``security.profile``.

    Looks only at *user* sources — the resolved user YAML file and
    ``ECHO_AGENT_*`` env vars — never the packaged default.yaml or schema
    defaults. Pure: reads inputs, mutates nothing.
    """
    path = resolve_config_file(config_path)
    user_yaml = _load_yaml_file(path if path and path.exists() else None)
    if isinstance(user_yaml.get("security"), dict) and "profile" in user_yaml["security"]:
        return True
    env = _env_overrides()
    return isinstance(env.get("security"), dict) and "profile" in env["security"]


def migrate_heartbeat_config(data: dict[str, Any]) -> dict[str, Any]:
    """Compat-migrate the removed agent.heartbeat fields in raw config data.

    - on_uneditable: "every" -> verbosity "every_tool"; "first_only"/"off"
      have no clean mapping, so they are dropped and the schema default
      (key_milestones) applies. An INFO log tells the user what happened.
    - interval_sec -> min_interval_sec (rename).
    Extra/unknown keys are ignored by pydantic, so dropping is safe.
    """
    hb = data.get("agent", {}).get("heartbeat")
    if not isinstance(hb, dict):
        return data
    legacy = hb.pop("on_uneditable", None)
    if legacy is not None:
        if legacy == "every":
            hb.setdefault("verbosity", "every_tool")
            logger.debug("config: on_uneditable=every migrated to heartbeat.verbosity=every_tool")
        else:
            logger.debug(
                "config: on_uneditable={} is obsolete and ignored; heartbeat now adapts per channel capability",
                legacy,
            )
    if "interval_sec" in hb and "min_interval_sec" not in hb:
        hb["min_interval_sec"] = hb.pop("interval_sec")
        logger.debug("config: heartbeat.interval_sec migrated to min_interval_sec")
    else:
        hb.pop("interval_sec", None)
    return data


def load_config(
    config_path: str | Path | None = None,
    overrides: dict[str, Any] | None = None,
) -> Config:
    # Each source is normalized to field names before merging: the schema
    # accepts camelCase aliases too, and an un-normalized merge would keep both
    # spellings of the same setting as separate keys — letting the alias win and
    # silently dropping the override. See _canonicalize_keys.
    data: dict[str, Any] = _canonicalize_keys(Config, _load_yaml_file(_PACKAGED_DEFAULT_CONFIG))

    path = resolve_config_file(config_path)
    if path and path.exists():
        # debug, not info: load_config runs on every CLI invocation (status,
        # cost, deps...) and loguru's default sink is stderr, so an info line
        # here leaks into otherwise-clean command output. Gateway logging is
        # unaffected — only this per-command load message is quieted.
        logger.debug("Loading config from {}", path)
        data = _deep_merge(data, _canonicalize_keys(Config, _load_yaml_file(path)))

    env = _env_overrides()
    if env:
        data = _deep_merge(data, env)

    if overrides:
        data = _deep_merge(data, _canonicalize_keys(Config, overrides))

    from echo_agent.config.profile_defaults import apply_profile_cognitive_defaults

    data = apply_profile_cognitive_defaults(data)

    data = migrate_heartbeat_config(data)

    try:
        return Config(**data)
    except Exception as e:
        # Translate pydantic validation errors into something a user editing
        # YAML can act on, instead of a raw traceback.
        details = []
        for err in getattr(e, "errors", lambda: [])():
            loc = ".".join(str(part) for part in err.get("loc", ()))
            details.append(f"  - {loc}: {err.get('msg', 'invalid value')}")
        summary = "\n".join(details) if details else f"  - {e}"
        source = f" loaded from {path}" if path else ""
        raise ConfigError(f"Invalid configuration{source}:\n{summary}") from e


def save_config(data: dict[str, Any], path: str | Path | None = None) -> Path:
    """Atomically write a configuration dict to a YAML file."""
    target = Path(path).expanduser() if path else default_config_path()
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(
        prefix=f".{target.name}.",
        suffix=".tmp",
        dir=target.parent,
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            yaml.dump(data, f, default_flow_style=False, allow_unicode=True, sort_keys=False)
            f.flush()
            os.fsync(f.fileno())
        os.replace(temporary, target)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            # Preserve the original save error; orphan cleanup is best-effort.
            pass
        raise
    return target
