"""CLI subcommand: echo-agent checkpoint list|show|restore|prune."""
from __future__ import annotations

import asyncio
import sys
from datetime import datetime
from pathlib import Path

from echo_agent.checkpoint.manager import CheckpointManager, snapshot_exclude
from echo_agent.checkpoint.store import ShadowGitStore


def _resolve_config_and_ws(config_path: str | None, workspace: str | None):
    from echo_agent.cli.plugins_cmd import _get_config_and_workspace
    return _get_config_and_workspace(config_path, workspace)


def _build_manager(
    store_path: str, workspace: Path, exclude: tuple[str, ...] = ()
) -> CheckpointManager:
    store = ShadowGitStore(Path(store_path).expanduser(), exclude=exclude)
    return CheckpointManager(store=store, workspace=workspace)


def run_checkpoint_command(
    action: str, sha: str = "", config_path: str | None = None,
    workspace: str | None = None, yes: bool = False,
) -> None:
    config, ws = _resolve_config_and_ws(config_path, workspace)
    mgr = _build_manager(
        config.checkpoint.store_path, ws, snapshot_exclude(config, ws)
    )

    async def _init() -> None:
        await mgr._store.ensure_initialized()

    asyncio.run(_init())

    if action == "list":
        snaps = asyncio.run(mgr.list_snapshots())
        if not snaps:
            print("No checkpoints for this workspace.")
            return
        for s in snaps:
            ts = datetime.fromtimestamp(s["ts"]).strftime("%Y-%m-%d %H:%M:%S")
            print(f"{s['short']}  {ts}  files={s['files']}  {s['subject']}")
    elif action == "show":
        if not sha:
            print("Usage: echo-agent checkpoint show <commit>")
            sys.exit(1)
        try:
            print(asyncio.run(mgr.show(sha)))
        except ValueError as e:
            print(f"错误: {e}")
            sys.exit(1)
    elif action == "restore":
        if not sha:
            print("Usage: echo-agent checkpoint restore <commit>")
            sys.exit(1)
        if not yes:
            reply = input(f"Restore checkpoint {sha[:10]}? This overwrites the changed files. [y/N] ")
            if reply.strip().lower() not in {"y", "yes"}:
                print("Aborted.")
                return
        try:
            restored = asyncio.run(mgr.restore(sha))
        except ValueError as e:
            print(f"错误: {e}")
            sys.exit(1)
        print(f"Restored {len(restored)} file(s): {', '.join(restored) or '(none)'}")
    elif action == "prune":
        dropped = asyncio.run(mgr.prune_now())
        print(f"Pruned {dropped} old checkpoint(s).")
    else:
        print(f"Unknown checkpoint action: {action}")
        print("Available: list, show, restore, prune")
        sys.exit(1)
