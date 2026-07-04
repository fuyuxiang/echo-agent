"""CheckpointManager — turn-deduped snapshotting over a ShadowGitStore."""
from __future__ import annotations

from pathlib import Path

from loguru import logger

from echo_agent.checkpoint.store import ShadowGitStore


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
        self._checkpointed_turns: set[str] = set()

    def new_turn(self, turn_id: str) -> None:
        if turn_id and turn_id != self._turn_id:
            self._turn_id = turn_id

    async def ensure_checkpoint(self, reason: str) -> str | None:
        if not self._enabled:
            return None
        key = self._turn_id or "no-turn"
        if key in self._checkpointed_turns:
            return None
        self._checkpointed_turns.add(key)
        try:
            await self._store.ensure_initialized()
            sha = await self._store.take_snapshot(
                self._workspace, f"[{key}] {reason}", self._max_file_size_mb
            )
            if sha is not None:
                await self._cleanup()
            return sha
        except Exception as e:
            logger.debug("checkpoint snapshot failed (fail-open): {}", e)
            return None

    async def _cleanup(self) -> None:
        try:
            await self._store.prune(self._workspace, self._max_snapshots)
            if await self._store.total_size_mb() > self._max_total_size_mb:
                await self._store.gc()
        except Exception as e:
            logger.debug("checkpoint cleanup failed (fail-open): {}", e)

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
