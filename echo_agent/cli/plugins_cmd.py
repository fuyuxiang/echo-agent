"""CLI subcommand for plugin management."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from echo_agent.cli.colors import Colors, color, set_color_override


def run_plugin_command(
    action: str,
    name: str = "",
    config_path: str | None = None,
    workspace: str | None = None,
    as_json: bool = False,
) -> int:
    """Handle plugin CLI subcommands and return a process exit code.

    Returns instead of calling ``sys.exit`` so ``__main__`` owns the single
    exit point for every subcommand. ``as_json`` switches every action to a
    machine-readable document with ANSI forced off.
    """
    if as_json:
        set_color_override(False)
    try:
        return _run(action, name, config_path, workspace, as_json)
    finally:
        if as_json:
            set_color_override(None)


def _run(
    action: str, name: str, config_path: str | None, workspace: str | None, as_json: bool
) -> int:
    if action == "list":
        return _list_plugins(config_path, workspace, as_json)
    if action in ("info", "enable", "disable"):
        if not name:
            return _usage_error(f"Usage: echo-agent plugin {action} <name>", as_json)
        if action == "info":
            return _show_plugin_info(name, config_path, workspace, as_json)
        return _toggle_plugin(
            name, enable=(action == "enable"), config_path=config_path,
            workspace=workspace, as_json=as_json,
        )
    if action == "check":
        return _check_plugins(config_path, workspace, as_json)
    return _usage_error(
        f"Unknown plugin action: {action}", as_json,
        hint="Available: list, info, enable, disable, check",
    )


def _usage_error(message: str, as_json: bool, hint: str = "") -> int:
    """Report a usage problem in the caller's requested format; always rc=1."""
    if as_json:
        payload: dict[str, Any] = {"ok": False, "error": message}
        if hint:
            payload["hint"] = hint
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(message)
        if hint:
            print(hint)
    return 1


def _get_config_and_workspace(
    config_path: str | None, workspace: str | None
) -> tuple[Any, Path]:
    """Load config and resolve workspace via the one authoritative rule.

    Thin alias over :func:`echo_agent.cli.workspace.load_config_and_workspace`,
    kept because this name is patched in tests and referenced across the plugin
    actions."""
    from echo_agent.cli.workspace import load_config_and_workspace

    return load_config_and_workspace(config_path, workspace)


def _plugin_status(name: str, deny_set: set[str], allow_set: set[str]) -> str:
    if name in deny_set:
        return "disabled"
    if allow_set and name not in allow_set:
        return "filtered"
    return "available"


def _list_plugins(
    config_path: str | None, workspace: str | None, as_json: bool = False
) -> int:
    """List all discovered plugins and their status."""
    config, ws = _get_config_and_workspace(config_path, workspace)

    from echo_agent.plugins.loader import discover_all

    records = discover_all(workspace=ws, extra_dirs=config.plugins.extra_dirs)
    deny_set = set(config.plugins.deny)
    allow_set = set(config.plugins.allow)
    search_locations = [
        "pip entry_points: echo_agent.plugins group",
        "User dir: ~/.echo-agent/plugins/",
        f"Project dir: {ws}/plugins/",
    ]

    if as_json:
        print(json.dumps({
            "ok": True,
            "count": len(records),
            "search_locations": search_locations,
            "plugins": [
                {
                    "name": r.manifest.name,
                    "version": r.manifest.version,
                    "description": r.manifest.description or None,
                    "status": _plugin_status(r.manifest.name, deny_set, allow_set),
                    "source": r.source,
                    "path": str(r.path) if r.path else None,
                }
                for r in records
            ],
        }, ensure_ascii=False, indent=2))
        return 0

    if not records:
        print("No plugins discovered.")
        print("\nSearch locations:")
        for line in search_locations:
            print(f"  {line}")
        return 0

    # Status markers go through ``color()`` so NO_COLOR / non-TTY / --json all
    # strip the escapes instead of corrupting captured output.
    markers = {
        "available": color("[available]", Colors.GREEN),
        "disabled": color("[disabled] ", Colors.RED),
        "filtered": color("[filtered]", Colors.YELLOW),
    }

    print(f"Plugins ({len(records)} discovered):\n")
    for record in records:
        name = record.manifest.name
        version = record.manifest.version
        desc = record.manifest.description or "(no description)"
        status = _plugin_status(name, deny_set, allow_set)

        print(f"  {markers[status]}  {name:<30} v{version:<8} {desc}")
        print(f"             source: {record.source}", end="")
        if record.path:
            print(f"  path: {record.path}", end="")
        print()
    return 0


def _show_plugin_info(
    name: str, config_path: str | None, workspace: str | None, as_json: bool = False
) -> int:
    """Show detailed info about a specific plugin."""
    config, ws = _get_config_and_workspace(config_path, workspace)

    from echo_agent.plugins.loader import discover_all
    from echo_agent.plugins.manifest import check_required_env

    records = discover_all(workspace=ws, extra_dirs=config.plugins.extra_dirs)
    record = next((r for r in records if r.manifest.name == name), None)

    if record is None:
        return _usage_error(f"Plugin '{name}' not found.", as_json)

    m = record.manifest
    missing = check_required_env(m)

    if as_json:
        print(json.dumps({
            "ok": True,
            "plugin": {
                "name": m.name,
                "version": m.version,
                "description": m.description,
                "author": m.author,
                "kind": m.kind,
                "source": record.source,
                "path": str(record.path) if record.path else None,
                "requires_env": list(m.requires_env),
                "missing_env": list(missing),
                "provides": {
                    "tools": list(m.provides.tools),
                    "hooks": list(m.provides.hooks),
                },
                "depends_on": list(m.depends_on),
                "config_key": (f"plugins.config.{m.config_key}" if m.config_key else None),
                "status": _plugin_status(
                    m.name, set(config.plugins.deny), set(config.plugins.allow)
                ),
            },
        }, ensure_ascii=False, indent=2))
        return 0

    print(f"Plugin: {m.name}")
    print(f"  Version:     {m.version}")
    print(f"  Description: {m.description}")
    print(f"  Author:      {m.author}")
    print(f"  Kind:        {m.kind}")
    print(f"  Source:      {record.source}")
    if record.path:
        print(f"  Path:        {record.path}")
    if m.requires_env:
        print(f"  Requires env: {', '.join(m.requires_env)}")
    if m.provides.tools:
        print(f"  Provides tools: {', '.join(m.provides.tools)}")
    if m.provides.hooks:
        print(f"  Provides hooks: {', '.join(m.provides.hooks)}")
    if m.depends_on:
        print(f"  Depends on: {', '.join(m.depends_on)}")
    if m.config_key:
        print(f"  Config key: plugins.config.{m.config_key}")

    if missing:
        print(f"\n  WARNING: Missing env vars: {', '.join(missing)}")
    return 0


def _toggle_plugin(
    name: str, *, enable: bool, config_path: str | None,
    workspace: str | None = None, as_json: bool = False,
) -> int:
    """Add/remove a plugin from the deny list in config.

    workspace 参与配置文件定位(search_dir),避免不带 -c 时误写 ~/.echo-agent
    下的全局配置。enable 时若存在非空 allow 白名单,同时把插件补进 allow,
    否则 CLI 提示 enabled 但运行期仍被白名单挡下,与实际不符。"""
    from echo_agent.config.loader import resolve_config_file

    import yaml

    config_file = resolve_config_file(config_path, search_dir=workspace)
    if config_file is None or not config_file.exists():
        return _usage_error(
            "No config file found. Create echo-agent.yaml first.", as_json
        )

    content = config_file.read_text(encoding="utf-8")
    data = yaml.safe_load(content) or {}

    plugins_section = data.setdefault("plugins", {})
    deny_list = plugins_section.setdefault("deny", [])
    allow_list = plugins_section.get("allow") or []

    if enable:
        changed = False
        if name in deny_list:
            deny_list.remove(name)
            changed = True
        # 白名单非空时,仅移出 deny 不足以让插件加载,还需补进 allow。
        if allow_list and name not in allow_list:
            allow_list.append(name)
            plugins_section["allow"] = allow_list
            changed = True
        if changed:
            config_file.write_text(yaml.dump(data, default_flow_style=False, allow_unicode=True), encoding="utf-8")
            message = f"Plugin '{name}' enabled."
        else:
            message = f"Plugin '{name}' is already enabled."
    else:
        changed = name not in deny_list
        if changed:
            deny_list.append(name)
            config_file.write_text(yaml.dump(data, default_flow_style=False, allow_unicode=True), encoding="utf-8")
            message = f"Plugin '{name}' disabled (added to deny list)."
        else:
            message = f"Plugin '{name}' is already disabled."

    if as_json:
        print(json.dumps({
            "ok": True,
            "plugin": name,
            "enabled": enable,
            "changed": changed,
            "config_file": str(config_file),
            "message": message,
        }, ensure_ascii=False, indent=2))
    else:
        print(message)
    return 0


def _check_plugins(
    config_path: str | None, workspace: str | None, as_json: bool = False
) -> int:
    """Dry-run: verify all plugins can be loaded.

    返回退出码:全部 OK 返回 0,存在加载失败/缺依赖返回 1,便于 CI 门禁。"""
    config, ws = _get_config_and_workspace(config_path, workspace)

    from echo_agent.plugins.loader import discover_all, load_plugin_module, topological_sort
    from echo_agent.plugins.manifest import check_required_env

    records = discover_all(workspace=ws, extra_dirs=config.plugins.extra_dirs)
    records = topological_sort(records)

    if not as_json:
        print(f"Checking {len(records)} plugin(s)...\n")
    results: list[dict[str, Any]] = []
    ok_count = 0
    fail_count = 0

    for record in records:
        name = record.manifest.name
        missing = check_required_env(record.manifest)
        if missing:
            results.append({"name": name, "result": "skip",
                            "detail": f"missing env: {', '.join(missing)}"})
            if not as_json:
                print(f"  SKIP  {name} — missing env: {', '.join(missing)}")
            fail_count += 1
            continue
        try:
            load_plugin_module(record)
            results.append({"name": name, "result": "ok", "detail": None})
            if not as_json:
                print(f"  OK    {name}")
            ok_count += 1
        except Exception as e:
            results.append({"name": name, "result": "fail", "detail": str(e)})
            if not as_json:
                print(f"  FAIL  {name} — {e}")
            fail_count += 1

    if as_json:
        print(json.dumps({
            "ok": fail_count == 0,
            "checked": len(records),
            "ok_count": ok_count,
            "fail_count": fail_count,
            "plugins": results,
        }, ensure_ascii=False, indent=2))
    else:
        print(f"\nResult: {ok_count} OK, {fail_count} failed/skipped")
    return 1 if fail_count else 0
