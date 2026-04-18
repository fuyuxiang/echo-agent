"""Workspace manager — file operations, isolation, change tracking, upload/download mapping."""

from __future__ import annotations

import hashlib
import json
import shutil
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

from loguru import logger


@dataclass
class FileChange:
    path: str
    action: str  # created | modified | deleted
    timestamp: str = field(default_factory=lambda: datetime.now().isoformat())
    checksum: str = ""


class WorkspaceManager:
    """Manages file operations, workspace isolation, and change tracking.

    Each task can get its own isolated working directory.
    Tracks all file changes for audit and rollback.
    """

    def __init__(self, root: Path):
        self._root = root
        self._root.mkdir(parents=True, exist_ok=True)
        self._changes: list[FileChange] = []
        self._upload_map: dict[str, str] = {}  # external_id -> local_path
        self._task_dirs: dict[str, Path] = {}

    @property
    def root(self) -> Path:
        return self._root

    def create_task_dir(self, task_id: str) -> Path:
        task_dir = self._root / "tasks" / task_id
        task_dir.mkdir(parents=True, exist_ok=True)
        self._task_dirs[task_id] = task_dir
        return task_dir

    def get_task_dir(self, task_id: str) -> Path | None:
        return self._task_dirs.get(task_id)

    def cleanup_task_dir(self, task_id: str) -> None:
        task_dir = self._task_dirs.pop(task_id, None)
        if task_dir and task_dir.exists():
            shutil.rmtree(task_dir, ignore_errors=True)

    def read_file(self, path: str) -> str:
        full = self._resolve(path)
        return full.read_text(encoding="utf-8")

    def write_file(self, path: str, content: str) -> None:
        full = self._resolve(path)
        full.parent.mkdir(parents=True, exist_ok=True)
        existed = full.exists()
        full.write_text(content, encoding="utf-8")
        self._track(path, "modified" if existed else "created", content)

    def delete_file(self, path: str) -> bool:
        full = self._resolve(path)
        if full.exists():
            full.unlink()
            self._track(path, "deleted")
            return True
        return False

    def create_dir(self, path: str) -> None:
        full = self._resolve(path)
        full.mkdir(parents=True, exist_ok=True)

    def list_dir(self, path: str = ".") -> list[dict[str, str]]:
        full = self._resolve(path)
        if not full.is_dir():
            return []
        entries = []
        for item in sorted(full.iterdir()):
            entries.append({
                "name": item.name,
                "type": "dir" if item.is_dir() else "file",
                "size": str(item.stat().st_size) if item.is_file() else "",
            })
        return entries

    def register_upload(self, external_id: str, local_path: str) -> None:
        self._upload_map[external_id] = local_path

    def resolve_upload(self, external_id: str) -> str | None:
        return self._upload_map.get(external_id)

    def get_changes(self, since: datetime | None = None) -> list[FileChange]:
        if not since:
            return list(self._changes)
        cutoff = since.isoformat()
        return [c for c in self._changes if c.timestamp >= cutoff]

    def _resolve(self, path: str) -> Path:
        p = Path(path)
        if p.is_absolute():
            return p
        return self._root / p

    def _track(self, path: str, action: str, content: str = "") -> None:
        checksum = ""
        if content:
            checksum = hashlib.sha256(content.encode()).hexdigest()[:16]
        self._changes.append(FileChange(path=path, action=action, checksum=checksum))

    def export_changes_log(self) -> str:
        lines = []
        for c in self._changes:
            lines.append(f"[{c.timestamp}] {c.action}: {c.path}")
        return "\n".join(lines)
