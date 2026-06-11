"""Approval allowlist with three persistence levels: once, session, always."""

from __future__ import annotations

import json
import os
import threading
from enum import Enum
from pathlib import Path
from typing import Any

from loguru import logger


class ApprovalLevel(str, Enum):
    ONCE = "once"
    SESSION = "session"
    ALWAYS = "always"


class ApprovalAllowlist:
    """Manages session-scoped and permanent approval allowlists."""

    def __init__(self, store_path: Path | None = None):
        self._lock = threading.Lock()
        self._session_approved: dict[str, set[str]] = {}
        self._permanent: set[str] = set()
        self._store_path = store_path
        if store_path:
            self._load()

    def is_approved(self, session_key: str, pattern_key: str) -> bool:
        """Check if a pattern is approved at any level."""
        with self._lock:
            if pattern_key in self._permanent:
                return True
            session_set = self._session_approved.get(session_key, set())
            return pattern_key in session_set

    def approve(self, session_key: str, pattern_key: str, level: ApprovalLevel = ApprovalLevel.ONCE) -> None:
        """Record an approval at the specified persistence level."""
        if level == ApprovalLevel.ONCE:
            return
        with self._lock:
            if level == ApprovalLevel.SESSION:
                self._session_approved.setdefault(session_key, set()).add(pattern_key)
            elif level == ApprovalLevel.ALWAYS:
                self._permanent.add(pattern_key)
                self._session_approved.setdefault(session_key, set()).add(pattern_key)
                self._save()

    def clear_session(self, session_key: str) -> None:
        with self._lock:
            self._session_approved.pop(session_key, None)

    def _load(self) -> None:
        if not self._store_path or not self._store_path.exists():
            return
        try:
            data = json.loads(self._store_path.read_text(encoding="utf-8"))
            self._permanent = set(data.get("permanent", []))
        except Exception as e:
            logger.warning("Failed to load approval allowlist: {}", e)

    def _save(self) -> None:
        if not self._store_path:
            return
        try:
            self._store_path.parent.mkdir(parents=True, exist_ok=True)
            data = {"permanent": sorted(self._permanent)}
            tmp = self._store_path.with_suffix(".tmp")
            tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")
            os.replace(tmp, self._store_path)
        except Exception as e:
            logger.warning("Failed to save approval allowlist: {}", e)


def build_pattern_key(tool_name: str, arguments: dict[str, Any]) -> str:
    """Build a pattern key for allowlist matching.

    For exec tools: exec:<command_name>
    For code tools: code:<language>
    For other tools: tool:<tool_name>
    """
    if tool_name == "exec":
        command = str(arguments.get("command", "")).strip()
        cmd_name = command.split()[0].rsplit("/", 1)[-1] if command else "unknown"
        return f"exec:{cmd_name}"
    if tool_name == "execute_code":
        lang = str(arguments.get("language", "unknown"))
        return f"code:{lang}"
    if tool_name == "process":
        command = str(arguments.get("command", "")).strip()
        cmd_name = command.split()[0].rsplit("/", 1)[-1] if command else "unknown"
        return f"process:{cmd_name}"
    return f"tool:{tool_name}"
