"""Session management — isolation, persistence, expiry, and archival."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import tempfile
from collections import OrderedDict
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
        """Return unconsolidated messages for LLM input, aligned to a safe boundary.

        Ensures no orphaned tool-result messages appear without their
        preceding assistant(tool_calls) message.
        """
        unconsolidated = self.messages[self.last_consolidated:]
        sliced = unconsolidated[-max_messages:]
        # Skip orphaned tool results at the start — their paired
        # assistant(tool_calls) was already consolidated.
        start = 0
        while start < len(sliced) and sliced[start].get("role") == "tool":
            start += 1
        sliced = sliced[start:]
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
    """Manages conversation sessions with JSONL or SQLite persistence.

    Session keys follow the pattern `channel:chat_id` to ensure isolation
    across private chats, group chats, different channels, and cron jobs.
    """

    def __init__(self, sessions_dir: Path, expiry_hours: int = 72, archive_hours: int = 168, storage: Any = None):
        self.sessions_dir = sessions_dir
        self.sessions_dir.mkdir(parents=True, exist_ok=True)
        self._expiry_delta = timedelta(hours=expiry_hours)
        self._archive_delta = timedelta(hours=archive_hours)
        self._cache: OrderedDict[str, Session] = OrderedDict()
        self._max_cache_size = 200
        self._storage = storage
        self._lock = asyncio.Lock()
        self._session_locks: OrderedDict[str, asyncio.Lock] = OrderedDict()
        self._max_session_locks = 200

    def _session_path(self, key: str) -> Path:
        safe = key.replace(":", "_").replace("/", "_")
        return self.sessions_dir / f"{safe}.jsonl"

    async def acquire(self, key: str) -> asyncio.Lock:
        """Return a per-session lock for serializing concurrent access.

        LRU eviction MUST NOT remove a lock that is currently held — doing so
        would let a second caller create a fresh Lock for the same key while
        the original holder is still inside its critical section, breaking
        mutual exclusion. Held locks are skipped during eviction.
        """
        async with self._lock:
            if key not in self._session_locks:
                self._session_locks[key] = asyncio.Lock()
            self._session_locks.move_to_end(key)
            while len(self._session_locks) > self._max_session_locks:
                evicted = False
                for candidate_key in list(self._session_locks.keys()):
                    if candidate_key == key:
                        continue
                    candidate_lock = self._session_locks[candidate_key]
                    if not candidate_lock.locked():
                        del self._session_locks[candidate_key]
                        evicted = True
                        break
                if not evicted:
                    # All other locks are in use; allow the cache to grow rather
                    # than violate mutex semantics.
                    break
            return self._session_locks[key]

    async def get_or_create(self, key: str) -> Session:
        async with self._lock:
            if key in self._cache:
                session = self._cache[key]
                self._cache.move_to_end(key)
                if session.status == "expired":
                    session.status = "active"
                    session.updated_at = datetime.now()
                return session

            session = await self._load(key)
            if session is None:
                session = Session(key=key)
            self._cache[key] = session
            self._cache.move_to_end(key)
            while len(self._cache) > self._max_cache_size:
                # Never evict a session whose per-session lock is held: its
                # holder is mid-turn, so saving here would snapshot a
                # half-finished state (and could later overwrite the holder's
                # complete save with a stale one).
                evicted_key = None
                for candidate_key in self._cache:
                    if candidate_key == key:
                        continue
                    candidate_lock = self._session_locks.get(candidate_key)
                    if candidate_lock is not None and candidate_lock.locked():
                        continue
                    evicted_key = candidate_key
                    break
                if evicted_key is None:
                    break  # everything is busy; let the cache grow temporarily
                evicted = self._cache.pop(evicted_key)
                try:
                    if self._storage:
                        await self._save_to_storage(evicted)
                    else:
                        await self._save_to_file(evicted)
                except Exception as e:
                    logger.warning("Failed to save evicted session {}: {}", evicted.key, e)
            return session

    async def _load(self, key: str) -> Session | None:
        if self._storage:
            return await self._load_from_storage(key)
        return await self._load_from_file(key)

    async def _load_from_storage(self, key: str) -> Session | None:
        try:
            data = await self._storage.load_session(key)
            if not data:
                return None
            return Session(
                key=key,
                messages=data.get("messages", []),
                created_at=datetime.fromisoformat(data["created_at"]) if data.get("created_at") else datetime.now(),
                updated_at=datetime.fromisoformat(data["updated_at"]) if data.get("updated_at") else datetime.now(),
                metadata=data.get("metadata", {}),
                last_consolidated=data.get("last_consolidated", 0),
                status=data.get("status", "active"),
            )
        except Exception as e:
            logger.warning("Failed to load session {} from storage: {}", key, e)
            return None

    async def _load_from_file(self, key: str) -> Session | None:
        path = self._session_path(key)
        if not path.exists():
            return None
        try:
            import asyncio

            def _sync_load() -> Session | None:
                messages: list[dict[str, Any]] = []
                metadata: dict[str, Any] = {}
                created_at = None
                updated_at = None
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
                            updated_at = datetime.fromisoformat(data["updated_at"]) if data.get("updated_at") else None
                            last_consolidated = data.get("last_consolidated", 0)
                            status = data.get("status", "active")
                        else:
                            messages.append(data)

                return Session(
                    key=key,
                    messages=messages,
                    created_at=created_at or datetime.now(),
                    updated_at=updated_at or datetime.now(),
                    metadata=metadata,
                    last_consolidated=last_consolidated,
                    status=status,
                )

            return await asyncio.to_thread(_sync_load)
        except Exception as e:
            logger.warning("Failed to load session {}: {}", key, e)
            return None

    async def save(self, session: Session) -> None:
        async with self._lock:
            self._cache[session.key] = session
            self._cache.move_to_end(session.key)
        if self._storage:
            await self._save_to_storage(session)
        else:
            await self._save_to_file(session)

    async def _save_to_storage(self, session: Session) -> None:
        data = {
            "messages": session.messages,
            "created_at": session.created_at.isoformat(),
            "updated_at": session.updated_at.isoformat(),
            "metadata": session.metadata,
            "last_consolidated": session.last_consolidated,
            "status": session.status,
        }
        try:
            await self._storage.store_session(session.key, data)
        except Exception as e:
            logger.warning("Failed to save session {} to storage, falling back to file: {}", session.key, e)
            await self._save_to_file(session)

    async def _save_to_file(self, session: Session) -> None:
        import asyncio

        path = self._session_path(session.key)
        # Snapshot on the event loop before handing off to a thread: the
        # writer must not iterate a list another task may still append to.
        messages_snapshot = list(session.messages)
        meta = {
            "_type": "metadata",
            "key": session.key,
            "created_at": session.created_at.isoformat(),
            "updated_at": session.updated_at.isoformat(),
            "metadata": dict(session.metadata),
            "last_consolidated": session.last_consolidated,
            "status": session.status,
        }

        def _sync_save() -> None:
            fd, tmp = tempfile.mkstemp(dir=str(self.sessions_dir), prefix=".sess_", suffix=".tmp")
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    f.write(json.dumps(meta, ensure_ascii=False) + "\n")
                    for msg in messages_snapshot:
                        f.write(json.dumps(msg, ensure_ascii=False) + "\n")
                    f.flush()
                    os.fsync(f.fileno())
                os.replace(tmp, str(path))
            except BaseException:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass
                raise

        await asyncio.to_thread(_sync_save)

    async def expire_session(self, key: str) -> None:
        async with self._lock:
            session = self._cache.get(key)
            if session:
                session.status = "expired"
        if session:
            await self.save(session)

    async def archive_session(self, key: str) -> bool:
        if self._storage:
            async with self._lock:
                session = self._cache.get(key)
            if session is None:
                session = await self._load_from_storage(key)
            if session is None:
                return False
            session.status = "archived"
            session.updated_at = datetime.now()
            await self.save(session)
            async with self._lock:
                self._cache.pop(key, None)
            return True
        path = self._session_path(key)
        if not path.exists():
            return False
        archive_dir = self.sessions_dir / "archive"
        archive_dir.mkdir(exist_ok=True)
        shutil.move(str(path), str(archive_dir / path.name))
        async with self._lock:
            self._cache.pop(key, None)
        return True

    async def cleanup_expired(self) -> int:
        """Expire stale sessions and archive old expired ones. Returns count processed."""
        now = datetime.now()
        count = 0
        if self._storage:
            for item in await self.list_sessions_async():
                try:
                    updated = datetime.fromisoformat(item["updated_at"]) if item.get("updated_at") else now
                    status = item.get("status", "active")
                    key = item.get("key", "")
                    if not key or status == "archived":
                        continue
                    if status == "active" and (now - updated) > self._expiry_delta:
                        await self.expire_session(key)
                        count += 1
                    elif status == "expired" and (now - updated) > self._archive_delta:
                        await self.archive_session(key)
                        count += 1
                except Exception as e:
                    logger.debug("Error during storage session cleanup for {}: {}", item.get("key", ""), e)
                    continue
            return count
        for path in self.sessions_dir.glob("*.jsonl"):
            key = path.stem
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
                    await self.expire_session(key)
                    count += 1
                elif status == "expired" and (now - updated) > self._archive_delta:
                    await self.archive_session(key)
                    count += 1
            except Exception as e:
                logger.debug("Error during session cleanup for {}: {}", key, e)
                continue
        return count

    async def list_sessions_async(self) -> list[dict[str, Any]]:
        if self._storage and hasattr(self._storage, "list_sessions"):
            sessions = await self._storage.list_sessions()
            return sorted(sessions, key=lambda x: x.get("updated_at", ""), reverse=True)
        return self.list_sessions()

    def list_sessions(self) -> list[dict[str, Any]]:
        """Synchronous listing. With SQLite storage and a running event loop
        this can only see the in-memory cache (≤ max_cache_size entries) —
        prefer ``list_sessions_async`` for a complete listing."""
        if self._storage:
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:
                return asyncio.run(self.list_sessions_async())
            if loop.is_running():
                return sorted(
                    [
                        {
                            "key": session.key,
                            "status": session.status,
                            "created_at": session.created_at.isoformat(),
                            "updated_at": session.updated_at.isoformat(),
                            "metadata": session.metadata,
                            "message_count": len(session.messages),
                        }
                        for session in self._cache.values()
                    ],
                    key=lambda x: x.get("updated_at", ""),
                    reverse=True,
                )
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
            except Exception as e:
                logger.debug("Failed to read session file {}: {}", path.name, e)
                continue
        return sorted(sessions, key=lambda x: x.get("updated_at", ""), reverse=True)

    async def invalidate(self, key: str) -> None:
        async with self._lock:
            self._cache.pop(key, None)
