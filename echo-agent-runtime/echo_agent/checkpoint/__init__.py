"""Transparent shadow-git checkpoint safety net for file writes."""
from __future__ import annotations

import shutil
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.checkpoint.manager import CheckpointManager


def git_available() -> bool:
    """True if a usable ``git`` executable is on PATH."""
    return shutil.which("git") is not None


_MANAGER: "CheckpointManager | None" = None


def set_checkpoint_manager(mgr: "CheckpointManager | None") -> None:
    global _MANAGER
    _MANAGER = mgr


def get_checkpoint_manager() -> "CheckpointManager | None":
    return _MANAGER
