"""CheckpointManager — turn-deduped snapshotting over a ShadowGitStore."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.checkpoint.store import ShadowGitStore


def snapshot_exclude(config: Any, workspace: Path) -> tuple[str, ...]:
    """Workspace-relative paths the snapshotter must never capture.

    Derived from the live config so it tracks user overrides instead of
    hard-coding "data/". Storage dirs (SQLite DB, sessions, memory, logs) and
    the shadow git store's own dir are excluded: a filesystem snapshot of a
    live SQLite file is a torn read, and capturing the checkpoint repo itself
    is recursive. Trailing globs catch SQLite's WAL/shm/journal sidecars.
    """
    ws = Path(workspace).expanduser().resolve()
    names: list[str] = []
    try:
        candidates = [
            config.storage.database_path,
            config.storage.sessions_dir,
            config.storage.memory_dir,
            config.storage.logs_dir,
            config.checkpoint.store_path,
        ]
    except AttributeError:
        candidates = []
    for raw in candidates:
        # This derivation runs on the fail-open checkpoint install path, so a
        # malformed or partial config must degrade to "exclude nothing extra"
        # rather than abort the whole safety-net install.
        try:
            cand = Path(raw).expanduser()
            cand = cand if cand.is_absolute() else ws / cand
            rel = cand.resolve().relative_to(ws)
        except (TypeError, ValueError, OSError):
            continue  # bad value, or outside ws: git never stages it anyway
        if rel.parts:
            names.append(f"{rel.parts[0]}/")
    ordered = tuple(dict.fromkeys(names))
    return ordered + ("*.db-wal", "*.db-shm", "*.db-journal")


class CheckpointManager:
    def __init__(
        self, *, store: ShadowGitStore, workspace: Path,
        max_snapshots: int = 20, max_total_size_mb: int = 500,
        max_file_size_mb: int = 10, enabled: bool = True,
    ) -> None:
        self._store = store
        self._workspace = Path(workspace).expanduser().resolve()
        self._max_snapshots = max_snapshots
        self._max_total_size_mb = max_total_size_mb
        self._max_file_size_mb = max_file_size_mb
        self._enabled = enabled
        self._turn_id: str = ""
        # Whether the current turn has already produced a checkpoint. Reset on
        # every turn change so the dedup set can't grow unbounded and a reused
        # turn_id is never wrongly deduped against a past turn.
        self._checkpointed_this_turn = False

    def new_turn(self, turn_id: str) -> None:
        if turn_id and turn_id != self._turn_id:
            self._turn_id = turn_id
            self._checkpointed_this_turn = False

    async def ensure_checkpoint(self, reason: str) -> str | None:
        if not self._enabled:
            return None
        if self._checkpointed_this_turn:
            return None
        key = self._turn_id or "no-turn"
        try:
            await self._store.ensure_initialized()
            sha = await self._store.take_snapshot(
                self._workspace, f"[{key}] {reason}", self._max_file_size_mb
            )
        except Exception as e:
            # Transient git failure: don't mark the turn done so the next write
            # in the same turn can retry.
            logger.debug("checkpoint snapshot failed (fail-open): {}", e)
            return None
        # Snapshot completed (sha may be None when nothing changed): the turn is
        # covered either way, so mark it before any cleanup.
        self._checkpointed_this_turn = True
        if sha is None:
            return None
        # Cleanup's prune re-roots the kept commits, rewriting their SHAs
        # (including the one we just took). Re-read the live ref tip afterwards
        # so the returned sha is the one that survives and passes _assert_owned.
        pruned = await self._cleanup()
        if pruned:
            try:
                snaps = await self._store.list_snapshots(self._workspace)
                if snaps:
                    return snaps[0]["sha"]
            except Exception as e:
                logger.debug("checkpoint re-read after prune failed: {}", e)
        return sha

    async def _cleanup(self) -> bool:
        """Prune + optional gc. Returns True if a prune actually dropped snapshots."""
        try:
            dropped = await self._store.prune(self._workspace, self._max_snapshots)
            if await self._store.total_size_mb() > self._max_total_size_mb:
                await self._store.gc()
            return dropped > 0
        except Exception as e:
            logger.debug("checkpoint cleanup failed (fail-open): {}", e)
            return False

    async def ensure_store_ready(self) -> None:
        """Initialise the shadow store if it does not exist yet.

        Public entrypoint for callers (the ``checkpoint`` CLI) that need the
        store on disk before a read-only operation, so they don't have to reach
        into the private ``_store``.
        """
        await self._store.ensure_initialized()

    async def list_snapshots(self) -> list[dict]:
        if not self._enabled:
            return []
        return await self._store.list_snapshots(self._workspace)

    async def show(self, sha: str) -> str:
        return await self._store.show_snapshot(self._workspace, sha)

    async def restore(self, sha: str) -> list[str]:
        return await self._store.restore(self._workspace, sha)

    async def prune_now(self) -> int:
        return await self._store.prune(self._workspace, self._max_snapshots)
