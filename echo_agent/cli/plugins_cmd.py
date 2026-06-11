"""CLI subcommand for plugin management."""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any



def run_plugin_command(
    action: str,
    name: str = "",
    config_path: str | None = None,
    workspace: str | None = None,
) -> None:
    """Handle plugin CLI subcommands."""
    if action == "list":
        _list_plugins(config_path, workspace)
    elif action == "info":
        if not name:
            print("Usage: echo-agent plugin info <name>")
            sys.exit(1)
        _show_plugin_info(name, config_path, workspace)
    elif action == "enable":
        if not name:
            print("Usage: echo-agent plugin enable <name>")
            sys.exit(1)
        _toggle_plugin(name, enable=True, config_path=config_path)
    elif action == "disable":
        if not name:
            print("Usage: echo-agent plugin disable <name>")
            sys.exit(1)
        _toggle_plugin(name, enable=False, config_path=config_path)
    elif action == "check":
        _check_plugins(config_path, workspace)
    else:
        print(f"Unknown plugin action: {action}")
        print("Available: list, info, enable, disable, check")
        sys.exit(1)


def _get_config_and_workspace(
    config_path: str | None, workspace: str | None
) -> tuple[Any, Path]:
    """Load config and resolve workspace."""
    from echo_agent.config.loader import load_config, resolve_config_file

    config_file = resolve_config_file(config_path)
    overrides = {"workspace": workspace} if workspace else None
    config = load_config(config_path=config_file, overrides=overrides)

    ws = Path(config.workspace).expanduser().resolve()
    return config, ws


def _list_plugins(config_path: str | None, workspace: str | None) -> None:
    """List all discovered plugins and their status."""
    config, ws = _get_config_and_workspace(config_path, workspace)

    from echo_agent.plugins.loader import discover_all

    records = discover_all(workspace=ws, extra_dirs=config.plugins.extra_dirs)

    if not records:
        print("No plugins discovered.")
        print("\nSearch locations:")
        print("  pip entry_points: echo_agent.plugins group")
        print("  User dir: ~/.echo-agent/plugins/")
        print(f"  Project dir: {ws}/plugins/")
        return

    deny_set = set(config.plugins.deny)
    allow_set = set(config.plugins.allow)

    print(f"Plugins ({len(records)} discovered):\n")
    for record in records:
        name = record.manifest.name
        version = record.manifest.version
        desc = record.manifest.description or "(no description)"

        if name in deny_set:
            status = "disabled"
        elif allow_set and name not in allow_set:
            status = "filtered"
        else:
            status = "available"

        status_marker = {
            "available": "\033[32m[available]\033[0m",
            "disabled": "\033[31m[disabled] \033[0m",
            "filtered": "\033[33m[filtered]\033[0m",
        }.get(status, f"[{status}]")

        print(f"  {status_marker}  {name:<30} v{version:<8} {desc}")
        print(f"             source: {record.source}", end="")
        if record.path:
            print(f"  path: {record.path}", end="")
        print()


def _show_plugin_info(name: str, config_path: str | None, workspace: str | None) -> None:
    """Show detailed info about a specific plugin."""
    config, ws = _get_config_and_workspace(config_path, workspace)

    from echo_agent.plugins.loader import discover_all

    records = discover_all(workspace=ws, extra_dirs=config.plugins.extra_dirs)
    record = next((r for r in records if r.manifest.name == name), None)

    if record is None:
        print(f"Plugin '{name}' not found.")
        sys.exit(1)

    m = record.manifest
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

    from echo_agent.plugins.manifest import check_required_env
    missing = check_required_env(m)
    if missing:
        print(f"\n  WARNING: Missing env vars: {', '.join(missing)}")


def _toggle_plugin(name: str, *, enable: bool, config_path: str | None) -> None:
    """Add/remove a plugin from the deny list in config."""
    from echo_agent.config.loader import resolve_config_file

    import yaml

    config_file = resolve_config_file(config_path)
    if config_file is None:
        print("No config file found. Create echo-agent.yaml first.")
        sys.exit(1)

    content = config_file.read_text(encoding="utf-8")
    data = yaml.safe_load(content) or {}

    plugins_section = data.setdefault("plugins", {})
    deny_list = plugins_section.setdefault("deny", [])

    if enable:
        if name in deny_list:
            deny_list.remove(name)
            config_file.write_text(yaml.dump(data, default_flow_style=False, allow_unicode=True), encoding="utf-8")
            print(f"Plugin '{name}' enabled (removed from deny list).")
        else:
            print(f"Plugin '{name}' is not in the deny list.")
    else:
        if name not in deny_list:
            deny_list.append(name)
            config_file.write_text(yaml.dump(data, default_flow_style=False, allow_unicode=True), encoding="utf-8")
            print(f"Plugin '{name}' disabled (added to deny list).")
        else:
            print(f"Plugin '{name}' is already disabled.")


def _check_plugins(config_path: str | None, workspace: str | None) -> None:
    """Dry-run: verify all plugins can be loaded."""
    config, ws = _get_config_and_workspace(config_path, workspace)

    from echo_agent.plugins.loader import discover_all, load_plugin_module, topological_sort
    from echo_agent.plugins.manifest import check_required_env

    records = discover_all(workspace=ws, extra_dirs=config.plugins.extra_dirs)
    records = topological_sort(records)

    print(f"Checking {len(records)} plugin(s)...\n")
    ok_count = 0
    fail_count = 0

    for record in records:
        name = record.manifest.name
        missing = check_required_env(record.manifest)
        if missing:
            print(f"  SKIP  {name} — missing env: {', '.join(missing)}")
            fail_count += 1
            continue
        try:
            load_plugin_module(record)
            print(f"  OK    {name}")
            ok_count += 1
        except Exception as e:
            print(f"  FAIL  {name} — {e}")
            fail_count += 1

    print(f"\nResult: {ok_count} OK, {fail_count} failed/skipped")
