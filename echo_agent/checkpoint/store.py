"""ShadowGitStore — env-isolated git subprocess wrapper for checkpoints."""
from __future__ import annotations

import asyncio
import hashlib
import os
from pathlib import Path


class ShadowGitStore:
    """A single external bare-ish git repo driven via GIT_DIR/GIT_WORK_TREE.

    Never touches the user's own .git: all git metadata lives under store_path,
    the work tree is pointed at the caller's workspace via env vars only.
    """

    def __init__(self, store_path: Path) -> None:
        self._store = Path(store_path).expanduser().resolve()
        self._initialized = False

    def _workspace_hash(self, workspace: Path) -> str:
        raw = str(Path(workspace).expanduser().resolve())
        return hashlib.sha256(raw.encode()).hexdigest()[:16]

    def ref_for(self, workspace: Path) -> str:
        return f"refs/echo/{self._workspace_hash(workspace)}"

    def _index_path(self, workspace: Path) -> Path:
        return self._store / "indexes" / self._workspace_hash(workspace)

    def _env_for(self, workspace: Path) -> dict[str, str]:
        env = dict(os.environ)
        env["GIT_DIR"] = str(self._store)
        env["GIT_WORK_TREE"] = str(Path(workspace).expanduser().resolve())
        env["GIT_INDEX_FILE"] = str(self._index_path(workspace))
        return env

    async def ensure_initialized(self) -> None:
        if self._initialized:
            return
        self._store.mkdir(parents=True, exist_ok=True)
        (self._store / "indexes").mkdir(parents=True, exist_ok=True)
        if not (self._store / "objects").exists():
            proc = await asyncio.create_subprocess_exec(
                "git", "init", "--bare", str(self._store),
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            )
            await proc.communicate()
        self._initialized = True

    async def _run_git(
        self, args: list[str], workspace: Path | None = None, check: bool = True
    ) -> tuple[int, str, str]:
        env = self._env_for(workspace) if workspace is not None else dict(os.environ)
        if workspace is None:
            # Store-level commands must never inherit an external work tree/index.
            env.pop("GIT_WORK_TREE", None)
            env.pop("GIT_INDEX_FILE", None)
            env["GIT_DIR"] = str(self._store)
        proc = await asyncio.create_subprocess_exec(
            "git", *args, env=env,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        out, err = await proc.communicate()
        rc = proc.returncode or 0
        if check and rc != 0:
            raise RuntimeError(f"git {' '.join(args)} failed ({rc}): {err.decode(errors='replace')}")
        return rc, out.decode(errors="replace"), err.decode(errors="replace")
