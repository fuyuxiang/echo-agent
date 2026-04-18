"""Session management — isolation, persistence, expiry, and archival."""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from loguru import logger


@dataclass
class Session:
    """A conversation session with append-only message history."""

    key: str
    messages: list[dict[str, Any]] = field(default_factory=list)
    created_at: datetime = field(default_factory=datetime.now)
    updated_at: datetime = field(default_factory=datetime.now)
    metadata: dict[str, Any] = field(default_factory=dict)
    last_consolidated: int = 0
    status: str = "active"  # active | expired | archived

    def add_message(self, role: str, content: str, **kwargs: Any) -> None:
        msg = {
            "role": role,
            "content": content,
            "timestamp": datetime.now().isoformat(),
            **kwargs,
        }
        self.messages.append(msg)
        self.updated_at = datetime.now()

    def get_history(self, max_messages: int = 500) -> list[dict[str, Any]]:
        """Return unconsolidated messages for LLM input, aligned to a user turn."""
        unconsolidated = self.messages[self.last_consolidated:]
        sliced = unconsolidated[-max_messages:]
        for i, m in enumerate(sliced):
            if m.get("role") == "user":
                return sliced[i:]
        return sliced

    def clear(self) -> None:
        self.messages.clear()
        self.last_consolidated = 0
        self.updated_at = datetime.now()

    @property
    def is_expired(self) -> bool:
        return self.status == "expired"

    @property
    def message_count(self) -> int:
        return len(self.messages)


class SessionManager:
    """Manages conversation sessions with JSONL persistence.

    Session keys follow the pattern `channel:chat_id` to ensure isolation
    across private chats, group chats, different channels, and cron jobs.
    """

    def __init__(self, sessions_dir: Path, expiry_hours: int = 72, archive_hours: int = 168):
        self.sessions_dir = sessions_dir
        self.sessions_dir.mkdir(parents=True, exist_ok=True)
        self._expiry_delta = timedelta(hours=expiry_hours)
        self._archive_delta = timedelta(hours=archive_hours)
        self._cache: dict[str, Session] = {}

    def _session_path(self, key: str) -> Path:
        safe = key.replace(":", "_").replace("/", "_")
        return self.sessions_dir / f"{safe}.jsonl"

    def get_or_create(self, key: str) -> Session:
        if key in self._cache:
            session = self._cache[key]
            if session.status == "expired":
                session.status = "active"
                session.updated_at = datetime.now()
            return session

        session = self._load(key)
        if session is None:
            session = Session(key=key)
        self._cache[key] = session
        return session

    def _load(self, key: str) -> Session | None:
        path = self._session_path(key)
        if not path.exists():
            return None
        try:
            messages: list[dict[str, Any]] = []
            metadata: dict[str, Any] = {}
            created_at = None
            last_consolidated = 0
            status = "active"

            with open(path, encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    data = json.loads(line)
                    if data.get("_type") == "metadata":
                        metadata = data.get("metadata", {})
                        created_at = datetime.fromisoformat(data["created_at"]) if data.get("created_at") else None
                        last_consolidated = data.get("last_consolidated", 0)
                        status = data.get("status", "active")
                    else:
                        messages.append(data)

            return Session(
                key=key,
                messages=messages,
                created_at=created_at or datetime.now(),
                metadata=metadata,
                last_consolidated=last_consolidated,
                status=status,
            )
        except Exception as e:
            logger.warning("Failed to load session {}: {}", key, e)
            return None

    def save(self, session: Session) -> None:
        path = self._session_path(session.key)
        with open(path, "w", encoding="utf-8") as f:
            meta = {
                "_type": "metadata",
                "key": session.key,
                "created_at": session.created_at.isoformat(),
                "updated_at": session.updated_at.isoformat(),
                "metadata": session.metadata,
                "last_consolidated": session.last_consolidated,
                "status": session.status,
            }
            f.write(json.dumps(meta, ensure_ascii=False) + "\n")
            for msg in session.messages:
                f.write(json.dumps(msg, ensure_ascii=False) + "\n")
        self._cache[session.key] = session

    def expire_session(self, key: str) -> None:
        session = self._cache.get(key)
        if session:
            session.status = "expired"
            self.save(session)

    def archive_session(self, key: str) -> bool:
        path = self._session_path(key)
        if not path.exists():
            return False
        archive_dir = self.sessions_dir / "archive"
        archive_dir.mkdir(exist_ok=True)
        shutil.move(str(path), str(archive_dir / path.name))
        self._cache.pop(key, None)
        return True

    def reopen_session(self, key: str) -> Session | None:
        archive_path = self.sessions_dir / "archive" / self._session_path(key).name
        if archive_path.exists():
            target = self._session_path(key)
            shutil.move(str(archive_path), str(target))
        session = self._load(key)
        if session:
            session.status = "active"
            session.updated_at = datetime.now()
            self._cache[key] = session
            self.save(session)
        return session

    def cleanup_expired(self) -> int:
        """Expire stale sessions and archive old expired ones. Returns count processed."""
        now = datetime.now()
        count = 0
        for path in self.sessions_dir.glob("*.jsonl"):
            try:
                with open(path, encoding="utf-8") as f:
                    first = f.readline().strip()
                if not first:
                    continue
                data = json.loads(first)
                if data.get("_type") != "metadata":
                    continue
                updated = datetime.fromisoformat(data["updated_at"]) if data.get("updated_at") else now
                status = data.get("status", "active")
                key = data.get("key", path.stem.replace("_", ":", 1))

                if status == "active" and (now - updated) > self._expiry_delta:
                    self.expire_session(key)
                    count += 1
                elif status == "expired" and (now - updated) > self._archive_delta:
                    self.archive_session(key)
                    count += 1
            except Exception:
                continue
        return count

    def list_sessions(self) -> list[dict[str, Any]]:
        sessions = []
        for path in self.sessions_dir.glob("*.jsonl"):
            try:
                with open(path, encoding="utf-8") as f:
                    first = f.readline().strip()
                if not first:
                    continue
                data = json.loads(first)
                if data.get("_type") == "metadata":
                    sessions.append({
                        "key": data.get("key", path.stem),
                        "status": data.get("status", "active"),
                        "created_at": data.get("created_at"),
                        "updated_at": data.get("updated_at"),
                    })
            except Exception:
                continue
        return sorted(sessions, key=lambda x: x.get("updated_at", ""), reverse=True)

    def invalidate(self, key: str) -> None:
        self._cache.pop(key, None)
