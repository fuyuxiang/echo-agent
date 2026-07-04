"""Transparent shadow-git checkpoint safety net for file writes."""
from __future__ import annotations

import shutil


def git_available() -> bool:
    """True if a usable ``git`` executable is on PATH."""
    return shutil.which("git") is not None
