"""Static context cache — LRU cache for identity/capabilities/skills segments.

These segments change rarely (only when tools are registered/unregistered or
skills are added/removed), so caching avoids rebuilding them on every turn.
"""

from __future__ import annotations

import hashlib
from collections import OrderedDict
from dataclasses import dataclass


@dataclass(frozen=True)
class _CacheKey:
    session_id: str
    skills_hash: str
    capabilities_hash: str


class ContextCache:
    """LRU cache for static context segments (identity + capabilities + skills)."""

    def __init__(self, max_size: int = 64):
        self._max_size = max_size
        self._cache: OrderedDict[_CacheKey, str] = OrderedDict()

    @staticmethod
    def _hash_content(content: str) -> str:
        return hashlib.md5(content.encode(), usedforsecurity=False).hexdigest()[:12]

    def get(self, session_id: str, skills_context: str, capabilities_context: str) -> str | None:
        key = _CacheKey(
            session_id=session_id,
            skills_hash=self._hash_content(skills_context),
            capabilities_hash=self._hash_content(capabilities_context),
        )
        if key in self._cache:
            self._cache.move_to_end(key)
            return self._cache[key]
        return None

    def put(self, session_id: str, skills_context: str, capabilities_context: str, value: str) -> None:
        key = _CacheKey(
            session_id=session_id,
            skills_hash=self._hash_content(skills_context),
            capabilities_hash=self._hash_content(capabilities_context),
        )
        self._cache[key] = value
        self._cache.move_to_end(key)
        while len(self._cache) > self._max_size:
            self._cache.popitem(last=False)

    def invalidate(self, session_id: str) -> None:
        keys_to_remove = [k for k in self._cache if k.session_id == session_id]
        for k in keys_to_remove:
            del self._cache[k]

    def clear(self) -> None:
        self._cache.clear()
