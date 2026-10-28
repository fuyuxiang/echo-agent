"""pre_tool_call hook wiring for transparent checkpoints."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.checkpoint import git_available, set_checkpoint_manager
from echo_agent.checkpoint.manager import CheckpointManager, snapshot_exclude
from echo_agent.checkpoint.store import ShadowGitStore

WRITE_TOOLS: frozenset[str] = frozenset({"write_file", "edit_file", "patch"})


def make_pre_tool_checkpoint_hook(mgr: CheckpointManager):
    async def _hook(name: str, arguments: Any, exec_ctx: Any = None) -> None:
        try:
            mgr.new_turn(getattr(exec_ctx, "trace_id", "") or "")
            if name in WRITE_TOOLS:
                await mgr.ensure_checkpoint(f"before {name}")
        except Exception as e:  # fail-open, never block the tool
            logger.debug("checkpoint pre_tool hook failed (fail-open): {}", e)
        return None
    return _hook


def install_checkpoint(config: Any, workspace: Path, hook_registry: Any) -> CheckpointManager | None:
    cp = config.checkpoint
    if not cp.enabled:
        return None
    if not git_available():
        logger.warning("checkpoint enabled but git not found on PATH — checkpoints disabled")
        return None
    store = ShadowGitStore(
        Path(cp.store_path).expanduser(),
        exclude=snapshot_exclude(config, workspace),
    )
    mgr = CheckpointManager(
        store=store, workspace=Path(workspace),
        max_snapshots=cp.max_snapshots_per_workspace,
        max_total_size_mb=cp.max_total_size_mb,
        max_file_size_mb=cp.max_file_size_mb,
        enabled=True,
    )
    set_checkpoint_manager(mgr)
    hook_registry.register("pre_tool_call", make_pre_tool_checkpoint_hook(mgr), plugin="checkpoint")
    logger.info("Checkpoint safety net enabled (store={})", cp.store_path)
    return mgr
