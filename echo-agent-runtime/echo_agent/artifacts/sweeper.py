"""Conservative retention and quota sweeper for user artifact directories."""

from __future__ import annotations

import asyncio
import json
import re
import time
from datetime import datetime
from pathlib import Path

from loguru import logger

_HEX32 = re.compile(r"^[a-f0-9]{32}$")


def _entry(
    root: Path, session_dir: Path, artifact_dir: Path,
) -> tuple[float, int, Path, str] | None:
    if (
        session_dir.is_symlink() or artifact_dir.is_symlink()
        or not _HEX32.fullmatch(session_dir.name) or not _HEX32.fullmatch(artifact_dir.name)
        or not artifact_dir.is_dir()
    ):
        return None
    manifest_path = artifact_dir / "manifest.json"
    if not manifest_path.is_file() or manifest_path.is_symlink():
        return None
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if (
            manifest.get("version") != 1
            or manifest.get("artifact_id") != artifact_dir.name
            or manifest.get("session_hash") != session_dir.name
        ):
            return None
        state = str(manifest.get("state") or "")
        if state not in {"draft", "finalized"}:
            return None
        timestamp = datetime.fromisoformat(str(manifest.get("updated_at"))).timestamp()
        size = 0
        for path in artifact_dir.rglob("*"):
            if path.is_symlink():
                continue
            if path.is_file():
                size += path.stat().st_size
        artifact_dir.resolve().relative_to(root.resolve())
        return timestamp, size, artifact_dir, state
    except (OSError, ValueError, TypeError, json.JSONDecodeError):
        return None


def _remove_owned_dir(directory: Path) -> bool:
    """Delete only the two-level shape emitted by ArtifactStore; never follow links."""
    try:
        children = list(directory.iterdir())
        chunk_children: list[Path] = []
        # Preflight the entire shape before deleting anything. A foreign nested
        # directory must leave the owned files intact, not cause a half-delete.
        for child in children:
            if child.is_symlink() or child.is_file():
                continue
            if child.name != "chunks" or not child.is_dir():
                return False
            for chunk in child.iterdir():
                if not (chunk.is_symlink() or chunk.is_file()):
                    return False
                chunk_children.append(chunk)
        for chunk in chunk_children:
            chunk.unlink()
        for child in children:
            if child.name == "chunks" and child.is_dir() and not child.is_symlink():
                child.rmdir()
            else:
                child.unlink()
        directory.rmdir()
        return True
    except OSError as exc:
        logger.debug("artifact cleanup failed for {}: {}", directory, exc)
        return False


def sweep(root: Path, retention_days: int, max_total_mb: int) -> int:
    root = Path(root)
    if not root.is_dir() or root.is_symlink():
        return 0
    entries: list[tuple[float, int, Path, str]] = []
    for session_dir in root.iterdir():
        if session_dir.is_symlink() or not session_dir.is_dir() or not _HEX32.fullmatch(session_dir.name):
            continue
        for artifact_dir in session_dir.iterdir():
            item = _entry(root, session_dir, artifact_dir)
            if item is not None:
                entries.append(item)

    deleted = 0
    cutoff = time.time() - retention_days * 86400
    survivors: list[tuple[float, int, Path, str]] = []
    for timestamp, size, directory, state in entries:
        if timestamp < cutoff and _remove_owned_dir(directory):
            deleted += 1
        else:
            survivors.append((timestamp, size, directory, state))

    budget = max_total_mb * 1024 * 1024
    total = sum(size for _, size, _, _ in survivors)
    active_draft_cutoff = time.time() - 3600
    for timestamp, size, directory, state in sorted(survivors):
        if total <= budget:
            break
        # A currently-running generation updates its draft manifest on every
        # append. Let the store exceed the soft total cap temporarily rather
        # than race the writer and destroy a report being assembled.
        if state == "draft" and timestamp >= active_draft_cutoff:
            continue
        if _remove_owned_dir(directory):
            deleted += 1
            total -= size

    for session_dir in root.iterdir():
        if session_dir.is_symlink() or not _HEX32.fullmatch(session_dir.name):
            continue
        try:
            session_dir.rmdir()
        except OSError:
            # Expected for every non-empty/live session directory.  Artifact
            # cleanup is best-effort and must never remove an unowned child.
            pass
    return deleted


async def sweep_forever(
    root: Path, retention_days: int, max_total_mb: int, interval_hours: int,
) -> None:
    while True:
        try:
            deleted = await asyncio.to_thread(sweep, root, retention_days, max_total_mb)
            if deleted:
                logger.info("artifact cleanup removed {} user artifacts", deleted)
        except asyncio.CancelledError:
            raise
        except Exception as exc:  # pragma: no cover - perpetual service safety net
            logger.warning("artifact cleanup failed: {}", exc)
        await asyncio.sleep(max(1, interval_hours) * 3600)
