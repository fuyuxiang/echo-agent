"""The `config` command: dump / explain / validate / gen-docs.

dump/explain/validate are user-facing runtime commands. gen-docs is a
developer command that writes the reference docs (used by CI consistency
checks). None of these mutate runtime behaviour.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import yaml

from echo_agent.config.docgen import render_backlog, render_markdown, render_yaml
from echo_agent.config.loader import ConfigError, load_config, resolve_config_file
from echo_agent.config.metadata import FieldInfo, iter_fields
from echo_agent.config.schema import Config

SECRET_HINTS: tuple[str, ...] = ("key", "secret", "token", "password", "cred", "auth")


def _is_secret(key: str) -> bool:
    low = key.lower()
    return any(h in low for h in SECRET_HINTS)


def redact(data: Any) -> Any:
    if isinstance(data, dict):
        out: dict[str, Any] = {}
        for k, v in data.items():
            if _is_secret(k) and isinstance(v, str) and v:
                out[k] = "****"
            elif (
                _is_secret(k)
                and isinstance(v, list)
                and any(isinstance(x, str) and x for x in v)
            ):
                # 凭据列表(如 credentialPool 轮换密钥池)整体打码,
                # 仅替换非空字符串元素,空值保持原样。
                out[k] = ["****" if isinstance(x, str) and x else x for x in v]
            else:
                out[k] = redact(v)
        return out
    if isinstance(data, list):
        return [redact(x) for x in data]
    return data


def _container_prefix(path: str) -> str | None:
    """Return the container prefix before the first ``[]``/``{}`` marker.

    e.g. ``models.providers[].apiKey`` -> ``models.providers``. Returns None
    when the path carries no container marker.
    """
    for marker in ("[]", "{}"):
        idx = path.find(marker)
        if idx != -1:
            return path[:idx]
    return None


def known_paths() -> set[str]:
    """Camel + snake dotted paths of every schema field, for validate.

    Container element fields (paths carrying ``[]``/``{}`` markers) are not
    meaningful for comparing against user YAML keys -- the user writes the
    container key (``providers:``) followed by a list, never
    ``providers[].apiKey``. So we add the *container prefix* (the segment
    before the marker) as a known key and skip the marked element path itself.
    """
    paths: set[str] = set()
    for f in iter_fields(Config):
        prefix = _container_prefix(f.path)
        if prefix is not None:
            snake_prefix = _container_prefix(f.snake_path)
            paths.add(prefix)
            if snake_prefix is not None:
                paths.add(snake_prefix)
            continue
        paths.add(f.path)
        paths.add(f.snake_path)
    return paths


def _container_keys() -> set[str]:
    """Known container keys (camel + snake) whose values must not be walked."""
    keys: set[str] = set()
    for f in iter_fields(Config):
        prefix = _container_prefix(f.path)
        if prefix is not None:
            keys.add(prefix)
        snake_prefix = _container_prefix(f.snake_path)
        if snake_prefix is not None:
            keys.add(snake_prefix)
    return keys


def _field_by_key(key: str) -> FieldInfo | None:
    for f in iter_fields(Config):
        if key in (f.path, f.snake_path):
            return f
    return None


def _dump(fmt: str, config_path, workspace) -> int:
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    config = load_config(config_path=config_file)
    data = redact(config.model_dump(by_alias=True))
    if fmt == "json":
        print(json.dumps(data, indent=2, ensure_ascii=False, default=str))
    else:
        print(yaml.dump(data, default_flow_style=False, allow_unicode=True, sort_keys=False))
    return 0


def _explain(key: str, config_path, workspace) -> int:
    info = _field_by_key(key)
    if info is None:
        print(f"未知配置项 / unknown key: {key}")
        return 1
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    config = load_config(config_path=config_file)
    current: Any = config
    for part in info.snake_path.split("."):
        current = getattr(current, part, None)
        if current is None:
            break
    status = info.extra.get("status")
    print(f"配置项 / key:   {info.path}  ({info.snake_path})")
    print(f"类型 / type:    {info.type_str}")
    print(f"默认值 / def:   {info.default!r}")
    print(f"当前值 / now:   {current!r}")
    if info.choices:
        print(f"可选 / choices: {'|'.join(info.choices)}")
    print(f"说明 / desc:    {info.extra.get('desc_zh', '')}")
    print(f"                {info.extra.get('desc_en', '')}")
    print(f"状态 / status:  {status or 'effective'}")
    if status == "dead":
        print(f"⚠ 此项当前未生效 / not in effect: {info.extra.get('reason', '')}")
    return 0


def _validate(config_path, workspace) -> int:
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    raw: dict[str, Any] = {}
    if config_file and Path(config_file).exists():
        with open(config_file, encoding="utf-8") as fh:
            raw = yaml.safe_load(fh) or {}
    # 1) schema 校验
    try:
        load_config(config_path=config_file)
    except ConfigError as e:
        print(f"配置非法 / invalid:\n{e}")
        return 1
    rc = 0
    known = known_paths()
    containers = _container_keys()
    dead_paths = {f.snake_path: f for f in iter_fields(Config)
                  if f.extra.get("status") == "dead"}
    dead_camel = {f.path: f for f in iter_fields(Config)
                  if f.extra.get("status") == "dead"}

    # 2) 未知字段检测 + 3) 死字段提示(扁平化用户键)
    def walk(node: Any, prefix: str) -> None:
        nonlocal rc
        if not isinstance(node, dict):
            return
        for k, v in node.items():
            path = f"{prefix}.{k}" if prefix else k
            # 已知容器键当作叶子式已知键处理,不下钻其值
            if path in containers:
                continue
            if isinstance(v, dict):
                # 仅当 path 是已知中间组才继续下钻;否则报未知
                if any(p == path or p.startswith(path + ".") for p in known):
                    walk(v, path)
                elif path not in known:
                    print(f"未知配置项 / unknown: {path}")
                    rc = 1
                continue
            if path not in known:
                print(f"未知配置项 / unknown: {path}")
                rc = 1
            elif path in dead_paths or path in dead_camel:
                info = dead_paths.get(path) or dead_camel[path]
                print(f"⚠ {path} 已设置但未生效 / set but not in effect: {info.extra.get('reason','')}")

    walk(raw, "")
    if rc == 0:
        print("配置有效 / configuration valid")
    return rc


def gen_docs(out_dir: str = "docs") -> None:
    """Developer command: write the reference files and dead-field backlog."""
    base = Path(out_dir)
    base.mkdir(parents=True, exist_ok=True)
    (base / "config-reference.yaml").write_text(render_yaml("zh"), encoding="utf-8")
    (base / "config-reference.en.yaml").write_text(render_yaml("en"), encoding="utf-8")
    (base / "config-reference.md").write_text(render_markdown("zh"), encoding="utf-8")
    (base / "config-reference.en.md").write_text(render_markdown("en"), encoding="utf-8")
    (base / "config-dead-fields-backlog.md").write_text(render_backlog(), encoding="utf-8")


def run_config_command(
    action: str,
    key: str = "",
    *,
    fmt: str = "yaml",
    config_path=None,
    workspace=None,
) -> int:
    if action == "dump":
        return _dump(fmt, config_path, workspace)
    if action == "explain":
        if not key:
            print("用法 / usage: config explain <key>")
            return 1
        return _explain(key, config_path, workspace)
    if action == "validate":
        return _validate(config_path, workspace)
    if action == "gen-docs":
        gen_docs()
        print("已生成配置参考文档 / reference docs generated")
        return 0
    print(f"未知子命令 / unknown action: {action}")
    return 1
