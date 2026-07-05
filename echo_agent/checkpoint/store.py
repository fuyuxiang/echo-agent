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
        self, args: list[str], workspace: Path | None = None, check: bool = True,
        extra_env: dict[str, str] | None = None,
    ) -> tuple[int, str, str]:
        env = self._env_for(workspace) if workspace is not None else dict(os.environ)
        if workspace is None:
            # Store-level commands must never inherit an external work tree/index.
            env.pop("GIT_WORK_TREE", None)
            env.pop("GIT_INDEX_FILE", None)
            env["GIT_DIR"] = str(self._store)
        # Stamp a fixed identity so commit-tree never depends on the caller's
        # global git config (CI runners, containers, and fresh machines have
        # none -> "Author identity unknown"). This store is internal-only, so a
        # constant identity keeps snapshots self-contained and reproducible.
        env.setdefault("GIT_AUTHOR_NAME", "echo-agent")
        env.setdefault("GIT_AUTHOR_EMAIL", "checkpoint@echo-agent.local")
        env.setdefault("GIT_COMMITTER_NAME", "echo-agent")
        env.setdefault("GIT_COMMITTER_EMAIL", "checkpoint@echo-agent.local")
        env.update(extra_env or {})
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
        # -z emits raw NUL-separated names; without it git C-quotes non-ASCII
        # names (e.g. Chinese), so ws / name would be a bogus quoted path.
        _, staged, _ = await self._run_git(
            ["diff", "--cached", "--name-only", "-z"], workspace=ws, check=False
        )
        for name in [n for n in staged.split("\x00") if n.strip()]:
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

    async def list_snapshots(self, workspace: Path, limit: int = 50) -> list[dict]:
        ws = Path(workspace).expanduser().resolve()
        ref = self.ref_for(ws)
        rc, _, _ = await self._run_git(["rev-parse", "--verify", ref], workspace=ws, check=False)
        if rc != 0:
            return []
        _, out, _ = await self._run_git(
            ["log", ref, f"-{limit}", "--pretty=format:%H%x1f%ct%x1f%s"], workspace=ws
        )
        snaps: list[dict] = []
        for line in [ln for ln in out.split("\n") if ln.strip()]:
            sha, ts, subject = line.split("\x1f", 2)
            files = len(await self.changed_paths(ws, sha))
            snaps.append({"sha": sha, "short": sha[:10], "ts": int(ts),
                          "subject": subject, "files": files})
        return snaps

    async def changed_paths(self, workspace: Path, sha: str) -> list[str]:
        ws = Path(workspace).expanduser().resolve()
        # -z keeps non-ASCII names raw; without it git C-quotes them (e.g.
        # Chinese -> "\346..."), and restore's checkout pathspec would miss.
        rc, out, _ = await self._run_git(
            ["diff-tree", "--no-commit-id", "--name-only", "-z", "-r", f"{sha}^", sha],
            workspace=ws, check=False,
        )
        if rc != 0:
            # no parent (first commit): list all files in the tree
            _, out, _ = await self._run_git(
                ["ls-tree", "-r", "--name-only", "-z", sha], workspace=ws
            )
        return [n for n in out.split("\x00") if n.strip()]

    async def show_snapshot(self, workspace: Path, sha: str) -> str:
        ws = Path(workspace).expanduser().resolve()
        await self._assert_owned(ws, sha)
        _, out, _ = await self._run_git(["show", sha], workspace=ws)
        return out

    async def _assert_owned(self, workspace: Path, sha: str) -> None:
        ref = self.ref_for(workspace)
        rc, _, _ = await self._run_git(
            ["merge-base", "--is-ancestor", sha, ref], workspace=workspace, check=False
        )
        # is-ancestor exits 0 when sha is reachable from ref
        _, tip, _ = await self._run_git(["rev-parse", "--verify", ref], workspace=workspace, check=False)
        if rc != 0 and sha != tip.strip():
            raise ValueError(f"checkpoint {sha[:10]} does not belong to this workspace")

    async def restore(self, workspace: Path, sha: str) -> list[str]:
        ws = Path(workspace).expanduser().resolve()
        await self._assert_owned(ws, sha)
        # pre-rollback snapshot so the rollback itself is undoable
        await self.take_snapshot(ws, f"before restore {sha[:10]}")
        paths = await self.changed_paths(ws, sha)
        if paths:
            await self._run_git(["checkout", sha, "--", *paths], workspace=ws)
        return paths

    async def prune(self, workspace: Path, max_snapshots: int) -> int:
        """Keep only the newest max_snapshots commits, dropping older ones.

        WARNING: this REWRITES the SHAs of the kept snapshots (re-root rebuild),
        so callers must NOT cache commit hashes obtained before prune. After
        prune, re-run list_snapshots to fetch the new SHAs.
        """
        ws = Path(workspace).expanduser().resolve()
        ref = self.ref_for(ws)
        _, out, _ = await self._run_git(
            ["log", ref, "--pretty=format:%H"], workspace=ws, check=False
        )
        shas = [s for s in out.split("\n") if s.strip()]
        if len(shas) <= max_snapshots:
            return 0
        # git log is newest-first. History is linear, so we can't keep the newest
        # N by moving the ref backward (that keeps the OLDEST N). Re-root instead:
        # rebuild the newest max_snapshots commits so the oldest kept one drops its
        # parent, orphaning everything older (gc reclaims those objects later).
        # Pass through each original author/committer date so list_snapshots ts
        # stays faithful instead of collapsing to the prune moment.
        keep = shas[:max_snapshots]  # newest-first
        new_parent: str | None = None
        for sha in reversed(keep):  # oldest kept first
            _, tree, _ = await self._run_git(
                ["rev-parse", f"{sha}^{{tree}}"], workspace=ws
            )
            _, meta, _ = await self._run_git(
                ["log", "-1", "--pretty=format:%at%x1f%ct%x1f%s", sha], workspace=ws
            )
            at, ct, subject = meta.split("\x1f", 2)
            parent_args = ["-p", new_parent] if new_parent else []
            _, new_sha, _ = await self._run_git(
                ["commit-tree", tree.strip(), *parent_args, "-m", subject],
                workspace=ws,
                extra_env={
                    "GIT_AUTHOR_DATE": f"{at} +0000",
                    "GIT_COMMITTER_DATE": f"{ct} +0000",
                },
            )
            new_parent = new_sha.strip()
        await self._run_git(["update-ref", ref, new_parent], workspace=ws)
        return len(shas) - max_snapshots

    async def total_size_mb(self) -> float:
        total = 0
        for p in self._store.rglob("*"):
            try:
                if p.is_file():
                    total += p.stat().st_size
            except OSError:
                continue
        return total / (1024 * 1024)

    async def gc(self) -> None:
        await self._run_git(["gc", "--auto", "--quiet"], check=False)
