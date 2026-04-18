"""Memory system — user memory, environment memory, CRUD, retrieval, consolidation.

Two-layer design:
  - User memory: preferences, habits, long-term requirements
  - Environment memory: project background, tool docs, process rules, domain knowledge

Supports: add / update / delete / conflict merge / keyword search /
          scored multi-keyword search / importance decay / injection into reasoning.
"""

from __future__ import annotations

import json
import math
import re
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any

from loguru import logger

_STOP_WORDS = frozenset({
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "shall", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "into", "about", "it", "its",
    "this", "that", "and", "or", "but", "not", "no", "if", "so", "than",
})


class MemoryType(str, Enum):
    USER = "user"
    ENVIRONMENT = "environment"


@dataclass
class MemoryEntry:
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])
    type: MemoryType = MemoryType.USER
    key: str = ""
    content: str = ""
    tags: list[str] = field(default_factory=list)
    source_session: str = ""
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())
    updated_at: str = field(default_factory=lambda: datetime.now().isoformat())
    importance: float = 0.5
    access_count: int = 0
    last_accessed: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "type": self.type.value,
            "key": self.key,
            "content": self.content,
            "tags": self.tags,
            "source_session": self.source_session,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "importance": self.importance,
            "access_count": self.access_count,
            "last_accessed": self.last_accessed,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> MemoryEntry:
        return cls(
            id=data.get("id", uuid.uuid4().hex[:12]),
            type=MemoryType(data.get("type", "user")),
            key=data.get("key", ""),
            content=data.get("content", ""),
            tags=data.get("tags", []),
            source_session=data.get("source_session", ""),
            created_at=data.get("created_at", ""),
            updated_at=data.get("updated_at", ""),
            importance=data.get("importance", 0.5),
            access_count=data.get("access_count", 0),
            last_accessed=data.get("last_accessed", ""),
        )

    def effective_importance(self, decay_half_life_days: float = 30.0) -> float:
        if not self.last_accessed or decay_half_life_days <= 0:
            return self.importance
        try:
            last = datetime.fromisoformat(self.last_accessed)
            days = (datetime.now() - last).total_seconds() / 86400
            decay = math.pow(0.5, days / decay_half_life_days)
            return self.importance * decay
        except (ValueError, OverflowError):
            return self.importance

    def touch(self) -> None:
        self.access_count += 1
        self.last_accessed = datetime.now().isoformat()


class MemoryStore:
    """Persistent memory store with file-based storage, importance decay, and scored search."""

    def __init__(self, memory_dir: Path, max_user: int = 1000, max_env: int = 500, decay_half_life_days: float = 30.0):
        self.memory_dir = memory_dir
        self.memory_dir.mkdir(parents=True, exist_ok=True)
        self._user_file = memory_dir / "user_memory.json"
        self._env_file = memory_dir / "env_memory.json"
        self._history_file = memory_dir / "HISTORY.md"
        self._long_term_file = memory_dir / "MEMORY.md"
        self._max_user = max_user
        self._max_env = max_env
        self._decay_half_life = decay_half_life_days
        self._entries: dict[str, MemoryEntry] = {}
        self._load()

    def _load(self) -> None:
        for path, mem_type in [(self._user_file, MemoryType.USER), (self._env_file, MemoryType.ENVIRONMENT)]:
            if not path.exists():
                continue
            try:
                data = json.loads(path.read_text(encoding="utf-8"))
                for item in data:
                    entry = MemoryEntry.from_dict(item)
                    entry.type = mem_type
                    self._entries[entry.id] = entry
            except Exception as e:
                logger.warning("Failed to load memory from {}: {}", path, e)

    def _save(self) -> None:
        user = [e.to_dict() for e in self._entries.values() if e.type == MemoryType.USER]
        env = [e.to_dict() for e in self._entries.values() if e.type == MemoryType.ENVIRONMENT]
        self._user_file.write_text(json.dumps(user, ensure_ascii=False, indent=2), encoding="utf-8")
        self._env_file.write_text(json.dumps(env, ensure_ascii=False, indent=2), encoding="utf-8")

    # ── CRUD ─────────────────────────────────────────────────────────────────

    def add(self, entry: MemoryEntry) -> MemoryEntry:
        existing = self._find_conflict(entry)
        if existing:
            return self.merge(existing.id, entry)
        limit = self._max_user if entry.type == MemoryType.USER else self._max_env
        count = sum(1 for e in self._entries.values() if e.type == entry.type)
        if count >= limit:
            self._evict_oldest(entry.type)
        self._entries[entry.id] = entry
        self._save()
        return entry

    def update(self, entry_id: str, content: str | None = None, tags: list[str] | None = None) -> MemoryEntry | None:
        entry = self._entries.get(entry_id)
        if not entry:
            return None
        if content is not None:
            entry.content = content
        if tags is not None:
            entry.tags = tags
        entry.updated_at = datetime.now().isoformat()
        self._save()
        return entry

    def delete(self, entry_id: str) -> bool:
        if entry_id in self._entries:
            del self._entries[entry_id]
            self._save()
            return True
        return False

    def get(self, entry_id: str) -> MemoryEntry | None:
        return self._entries.get(entry_id)

    def list_all(self, mem_type: MemoryType | None = None) -> list[MemoryEntry]:
        entries = list(self._entries.values())
        if mem_type:
            entries = [e for e in entries if e.type == mem_type]
        return sorted(entries, key=lambda e: e.updated_at, reverse=True)

    # ── Conflict merge ───────────────────────────────────────────────────────

    def _find_conflict(self, entry: MemoryEntry) -> MemoryEntry | None:
        for existing in self._entries.values():
            if existing.type == entry.type and existing.key == entry.key and entry.key:
                return existing
        return None

    def merge(self, existing_id: str, new_entry: MemoryEntry) -> MemoryEntry:
        existing = self._entries[existing_id]
        existing.content = new_entry.content
        existing.tags = list(set(existing.tags + new_entry.tags))
        existing.importance = max(existing.importance, new_entry.importance)
        existing.updated_at = datetime.now().isoformat()
        self._save()
        return existing

    def _evict_oldest(self, mem_type: MemoryType) -> None:
        typed = sorted(
            (e for e in self._entries.values() if e.type == mem_type),
            key=lambda e: (e.effective_importance(self._decay_half_life), e.updated_at),
        )
        if typed:
            del self._entries[typed[0].id]

    # ── Search ───────────────────────────────────────────────────────────────

    def search_keyword(self, query: str, mem_type: MemoryType | None = None, limit: int = 20) -> list[MemoryEntry]:
        pattern = re.compile(re.escape(query), re.IGNORECASE)
        results = []
        for entry in self._entries.values():
            if mem_type and entry.type != mem_type:
                continue
            if pattern.search(entry.content) or pattern.search(entry.key) or any(pattern.search(t) for t in entry.tags):
                entry.touch()
                results.append(entry)
        results.sort(key=lambda e: e.effective_importance(self._decay_half_life), reverse=True)
        if results:
            self._save()
        return results[:limit]

    def search_scored(self, query: str, mem_type: MemoryType | None = None, limit: int = 10) -> list[tuple[MemoryEntry, float]]:
        """Multi-keyword scored search. Returns (entry, score) pairs sorted by score."""
        words = [w.lower() for w in re.findall(r"\w+", query) if len(w) > 1 and w.lower() not in _STOP_WORDS]
        if not words:
            return [(e, e.effective_importance(self._decay_half_life)) for e in self.search_keyword(query, mem_type, limit)]

        scored: list[tuple[MemoryEntry, float]] = []
        for entry in self._entries.values():
            if mem_type and entry.type != mem_type:
                continue
            haystack = f"{entry.key} {entry.content} {' '.join(entry.tags)}".lower()
            word_hits = sum(1 for w in words if w in haystack)
            if word_hits == 0:
                continue
            coverage = word_hits / len(words)
            eff_imp = entry.effective_importance(self._decay_half_life)
            score = coverage * 0.7 + eff_imp * 0.3
            entry.touch()
            scored.append((entry, score))

        scored.sort(key=lambda x: x[1], reverse=True)
        if scored:
            self._save()
        return scored[:limit]

    def find_by_key(self, key: str, mem_type: MemoryType | None = None) -> MemoryEntry | None:
        """Find an entry by exact key match."""
        for entry in self._entries.values():
            if entry.key == key and (mem_type is None or entry.type == mem_type):
                return entry
        return None

    def find_by_content(self, substring: str, mem_type: MemoryType | None = None) -> MemoryEntry | None:
        """Find first entry whose content contains the substring."""
        for entry in self._entries.values():
            if mem_type and entry.type != mem_type:
                continue
            if substring in entry.content:
                return entry
        return None

    def search_by_time(
        self, start: datetime | None = None, end: datetime | None = None, mem_type: MemoryType | None = None,
    ) -> list[MemoryEntry]:
        results = []
        for entry in self._entries.values():
            if mem_type and entry.type != mem_type:
                continue
            ts = datetime.fromisoformat(entry.updated_at)
            if start and ts < start:
                continue
            if end and ts > end:
                continue
            results.append(entry)
        return sorted(results, key=lambda e: e.updated_at, reverse=True)

    # ── Context injection ────────────────────────────────────────────────────

    def get_context(self, mem_type: MemoryType | None = None, max_entries: int = 50) -> str:
        entries = self.list_all(mem_type)[:max_entries]
        if not entries:
            return ""
        entries.sort(key=lambda e: e.effective_importance(self._decay_half_life), reverse=True)
        lines = []
        for e in entries:
            tags = f" [{', '.join(e.tags)}]" if e.tags else ""
            lines.append(f"- **{e.key}**{tags}: {e.content}")
        return "\n".join(lines)

    def get_snapshot(self) -> str:
        """Build a frozen memory snapshot for system prompt injection."""
        parts: list[str] = []
        long_term = self.read_long_term()
        if long_term:
            parts.append(f"## Long-term Memory\n\n{long_term}")

        user_ctx = self.get_context(MemoryType.USER, max_entries=30)
        if user_ctx:
            parts.append(f"## User Memory\n\n{user_ctx}")

        env_ctx = self.get_context(MemoryType.ENVIRONMENT, max_entries=30)
        if env_ctx:
            parts.append(f"## Environment Memory\n\n{env_ctx}")

        return "\n\n".join(parts)

    # ── Long-term memory file (MEMORY.md) ────────────────────────────────────

    def read_long_term(self) -> str:
        if self._long_term_file.exists():
            return self._long_term_file.read_text(encoding="utf-8")
        return ""

    def write_long_term(self, content: str) -> None:
        self._long_term_file.write_text(content, encoding="utf-8")

    def append_history(self, entry: str) -> None:
        with open(self._history_file, "a", encoding="utf-8") as f:
            f.write(entry.rstrip() + "\n\n")

    def search_history(self, query: str, limit: int = 20) -> list[str]:
        if not self._history_file.exists():
            return []
        content = self._history_file.read_text(encoding="utf-8")
        entries = content.split("\n\n")
        pattern = re.compile(re.escape(query), re.IGNORECASE)
        return [e.strip() for e in entries if e.strip() and pattern.search(e)][:limit]
