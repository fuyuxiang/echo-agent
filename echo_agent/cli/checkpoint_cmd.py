"""CLI subcommand: echo-agent checkpoint list|show|restore|prune."""
from __future__ import annotations

import asyncio
import json
from datetime import datetime
from pathlib import Path

from echo_agent.checkpoint.manager import CheckpointManager, snapshot_exclude
from echo_agent.checkpoint.store import ShadowGitStore
from echo_agent.cli.colors import set_color_override


def _resolve_config_and_ws(config_path: str | None, workspace: str | None):
    from echo_agent.cli.workspace import load_config_and_workspace
    return load_config_and_workspace(config_path, workspace)


def _build_manager(
    store_path: str, workspace: Path, exclude: tuple[str, ...] = ()
) -> CheckpointManager:
    store = ShadowGitStore(Path(store_path).expanduser(), exclude=exclude)
    return CheckpointManager(store=store, workspace=workspace)


def run_checkpoint_command(
    action: str, sha: str = "", config_path: str | None = None,
    workspace: str | None = None, yes: bool = False, as_json: bool = False,
) -> int:
    """Handle checkpoint subcommands and return a process exit code.

    ``as_json`` emits a machine-readable document with ANSI forced off. In JSON
    mode ``restore`` requires ``yes`` — prompting would corrupt the document and
    a script has no way to answer.
    """
    if as_json:
        set_color_override(False)
    try:
        return _run(action, sha, config_path, workspace, yes, as_json)
    finally:
        if as_json:
            set_color_override(None)


def _emit_error(message: str, as_json: bool, hint: str = "") -> int:
    if as_json:
        payload = {"ok": False, "error": message}
        if hint:
            payload["hint"] = hint
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(message)
        if hint:
            print(hint)
    return 1


def _run(
    action: str, sha: str, config_path: str | None, workspace: str | None,
    yes: bool, as_json: bool,
) -> int:
    config, ws = _resolve_config_and_ws(config_path, workspace)
    mgr = _build_manager(
        config.checkpoint.store_path, ws, snapshot_exclude(config, ws)
    )

    asyncio.run(mgr.ensure_store_ready())

    if action == "list":
        snaps = asyncio.run(mgr.list_snapshots())
        if as_json:
            print(json.dumps({
                "ok": True,
                "count": len(snaps),
                "checkpoints": [
                    {
                        "short": s["short"],
                        "ts": s["ts"],
                        "time": datetime.fromtimestamp(s["ts"]).isoformat(timespec="seconds"),
                        "files": s["files"],
                        "subject": s["subject"],
                    }
                    for s in snaps
                ],
            }, ensure_ascii=False, indent=2))
            return 0
        if not snaps:
            print("No checkpoints for this workspace.")
            return 0
        for s in snaps:
            ts = datetime.fromtimestamp(s["ts"]).strftime("%Y-%m-%d %H:%M:%S")
            print(f"{s['short']}  {ts}  files={s['files']}  {s['subject']}")
        return 0

    if action == "show":
        if not sha:
            return _emit_error("Usage: echo-agent checkpoint show <commit>", as_json)
        try:
            diff = asyncio.run(mgr.show(sha))
        except ValueError as e:
            return _emit_error(f"错误: {e}", as_json)
        if as_json:
            print(json.dumps({"ok": True, "sha": sha, "diff": diff},
                             ensure_ascii=False, indent=2))
        else:
            print(diff)
        return 0

    if action == "restore":
        if not sha:
            return _emit_error("Usage: echo-agent checkpoint restore <commit>", as_json)
        if not yes:
            if as_json:
                return _emit_error(
                    "restore in --json mode requires -y/--yes (cannot prompt).", as_json
                )
            # Goes through prompt_yes_no rather than a bare input() so a piped /
            # non-TTY invocation fails loudly instead of raising EOFError while
            # about to overwrite the user's files. An EOF is NOT consent and it
            # is not a decline either - nothing restored, so report non-zero so
            # a wrapper script cannot read "exit 0" as "restored".
            from echo_agent.cli.prompt import PromptAborted, prompt_yes_no
            try:
                confirmed = prompt_yes_no(
                    f"Restore checkpoint {sha[:10]}? This overwrites the changed files.",
                    default=False,
                )
            except PromptAborted:
                return _emit_error(
                    "restore needs a confirmation; pass -y/--yes for "
                    "non-interactive use.",
                    as_json,
                )
            if not confirmed:
                print("Aborted.")
                return 0
        try:
            restored = asyncio.run(mgr.restore(sha))
        except ValueError as e:
            return _emit_error(f"错误: {e}", as_json)
        if as_json:
            print(json.dumps({"ok": True, "sha": sha, "restored": list(restored)},
                             ensure_ascii=False, indent=2))
        else:
            print(f"Restored {len(restored)} file(s): {', '.join(restored) or '(none)'}")
        return 0

    if action == "prune":
        dropped = asyncio.run(mgr.prune_now())
        if as_json:
            print(json.dumps({"ok": True, "pruned": dropped},
                             ensure_ascii=False, indent=2))
        else:
            print(f"Pruned {dropped} old checkpoint(s).")
        return 0

    return _emit_error(
        f"Unknown checkpoint action: {action}", as_json,
        hint="Available: list, show, restore, prune",
    )
