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

    async def take_snapshot(
        self, workspace: Path, message: str, max_file_size_mb: int = 10
    ) -> str | None:
        ws = Path(workspace).expanduser().resolve()
        ref = self.ref_for(ws)
        # Load previous tip into this workspace's index (empty tree if first run).
        rc, _, _ = await self._run_git(["rev-parse", "--verify", ref], workspace=ws, check=False)
        if rc == 0:
            await self._run_git(["read-tree", ref], workspace=ws)
        else:
            await self._run_git(["read-tree", "--empty"], workspace=ws)
        # Stage everything, then unstage oversize blobs.
        await self._run_git(["add", "-A"], workspace=ws)
        limit = max_file_size_mb * 1024 * 1024
        _, staged, _ = await self._run_git(
            ["diff", "--cached", "--name-only"], workspace=ws, check=False
        )
        for name in [n for n in staged.split("\n") if n.strip()]:
            fp = ws / name
            try:
                if fp.is_file() and fp.stat().st_size > limit:
                    await self._run_git(["rm", "--cached", "--", name], workspace=ws, check=False)
            except OSError:
                continue
        # Write the staged tree, then compare it against the parent commit's tree.
        _, tree, _ = await self._run_git(["write-tree"], workspace=ws)
        tree = tree.strip()
        parent_args: list[str] = []
        rc_ref, parent, _ = await self._run_git(
            ["rev-parse", "--verify", ref], workspace=ws, check=False
        )
        if rc_ref == 0:
            parent = parent.strip()
            # diff-tree --quiet exits 0 when the trees are identical -> no change, skip.
            rc_id, _, _ = await self._run_git(
                ["diff-tree", "--quiet", parent, tree], workspace=ws, check=False
            )
            if rc_id == 0:
                return None
            parent_args = ["-p", parent]
        _, sha, _ = await self._run_git(
            ["commit-tree", tree, *parent_args, "-m", message], workspace=ws
        )
        sha = sha.strip()
        await self._run_git(["update-ref", ref, sha], workspace=ws)
        return sha
