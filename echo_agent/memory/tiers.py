from __future__ import annotations
import json
import uuid
from datetime import datetime
from typing import Any, TYPE_CHECKING
from loguru import logger

if TYPE_CHECKING:
    from echo_agent.storage.backend import StorageBackend

from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType, Episode


class WorkingMemory:
    """In-process buffer for current conversation context. Not persisted."""

    def __init__(self, max_entries: int = 20):
        self._max = max_entries
        self._entries: list[MemoryEntry] = []

    def load(self, entries: list[MemoryEntry]) -> None:
        """Replace working memory with given entries."""
        self._entries = entries[:self._max]

    def add(self, entry: MemoryEntry) -> None:
        """Add entry, evicting oldest if at capacity."""
        if len(self._entries) >= self._max:
            self._entries.pop(0)
        entry.tier = MemoryTier.WORKING
        self._entries.append(entry)

    def get_context(self, max_chars: int = 2000) -> str:
        """Render working memory as markdown for prompt injection."""
        if not self._entries:
            return ""
        lines = []
        used = 0
        for entry in reversed(self._entries):
            line = f"- {entry.key}: {entry.content}" if entry.key else f"- {entry.content}"
            if used + len(line) + 1 > max_chars:
                break
            lines.append(line)
            used += len(line) + 1
        return "\n".join(reversed(lines))

    def clear(self) -> None:
        self._entries.clear()

    @property
    def entries(self) -> list[MemoryEntry]:
        return list(self._entries)

    @property
    def count(self) -> int:
        return len(self._entries)


class EpisodicManager:
    """Manages conversation episodes with temporal indexing.

    With attach_embedding(), episode summaries are embedded into the shared
    vectors table under 'ep:<episode_id>' source_ids so search_episodes can
    do semantic matching (falls back to LIKE when embedding is unavailable)."""

    def __init__(self, storage: StorageBackend):
        self._storage = storage
        self._embed_fn = None
        self._vector_index = None

    def attach_embedding(self, embed_fn, vector_index) -> None:
        self._embed_fn = embed_fn
        self._vector_index = vector_index

    async def create_episode(
        self,
        session_key: str,
        messages: list[dict[str, Any]],
        summary: str,
        entity_ids: list[str] | None = None,
        importance: float = 0.5,
        message_range: tuple[int, int] = (0, 0),
    ) -> Episode:
        episode = Episode(
            id=uuid.uuid4().hex[:12],
            session_key=session_key,
            summary=summary,
            message_range_start=message_range[0],
            message_range_end=message_range[1],
            entity_ids=entity_ids or [],
            importance=importance,
            created_at=datetime.now().isoformat(),
        )
        await self._storage.execute_sql(
            "INSERT OR REPLACE INTO memory_episodes (id, session_key, summary, "
            "message_range_start, message_range_end, entities, importance, created_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                episode.id, episode.session_key, episode.summary,
                episode.message_range_start, episode.message_range_end,
                json.dumps(episode.entity_ids), episode.importance, episode.created_at,
            ),
        )
        await self._embed_summary(episode.id, summary)
        logger.debug("Created episode {} for session {}", episode.id, session_key)
        return episode

    async def _embed_summary(self, episode_id: str, summary: str) -> bool:
        """Best-effort embed; failure degrades that episode to LIKE-only.
        Returns True only when a new vector row was actually added."""
        if not self._embed_fn or not self._vector_index or not summary:
            return False
        try:
            embedding = await self._embed_fn(summary)
            if not embedding:
                return False
            vec_id = await self._vector_index.add(f"ep:{episode_id}", embedding)
            return bool(vec_id)
        except Exception as e:
            logger.debug("Episode embedding failed for {}: {}", episode_id, e)
            return False

    async def requeue_stale(self, stale_source_ids: set) -> int:
        """Re-embed episodes whose vector came from a different model.
        Embed the new vector FIRST; only delete the stale row once the new
        one is stored. If the re-embed fails the old row stays put so the next
        startup surfaces it in stale_source_ids again for another retry — a
        failed re-embed must never leave the episode with zero vectors."""
        if not self._embed_fn or not self._vector_index:
            return 0
        count = 0
        for source_id in stale_source_ids:
            if not source_id.startswith("ep:"):
                continue
            episode_id = source_id[3:]
            rows = await self._storage.fetch_sql(
                "SELECT summary FROM memory_episodes WHERE id = ?", (episode_id,),
            )
            if not rows:
                continue
            old = await self._storage.load_vector_by_source(source_id)
            # Add the new vector first; keep the stale row until it succeeds so
            # the episode always has at least one vector at any instant.
            embedded = await self._embed_summary(episode_id, rows[0].get("summary", ""))
            if not embedded:
                continue  # keep stale row; will retry next startup
            if old:
                await self._storage.delete_vector(old["id"])
            count += 1
        if count:
            logger.info("Re-embedded {} stale episode vectors", count)
        return count

    async def search_episodes(
        self, query: str, session_key: str | None = None, limit: int = 5,
    ) -> list[Episode]:
        semantic = await self._semantic_search(query, session_key, limit)
        if semantic:
            return semantic
        return await self._like_search(query, session_key, limit)

    async def _semantic_search(
        self, query: str, session_key: str | None, limit: int,
    ) -> list[Episode]:
        if not self._embed_fn or not self._vector_index:
            return []
        try:
            embedding = await self._embed_fn(query)
            if not embedding:
                return []
            hits = await self._vector_index.search(embedding, limit=limit * 4)
        except Exception as e:
            logger.debug("Episodic semantic search failed: {}", e)
            return []
        episode_ids = [sid[3:] for sid, _ in hits if sid.startswith("ep:")]
        if not episode_ids:
            return []
        placeholders = ",".join("?" for _ in episode_ids)
        rows = await self._storage.fetch_sql(
            f"SELECT * FROM memory_episodes WHERE id IN ({placeholders})",
            tuple(episode_ids),
        )
        by_id = {r["id"]: self._row_to_episode(r) for r in rows}
        ordered = [by_id[eid] for eid in episode_ids if eid in by_id]
        if session_key:
            ordered = [ep for ep in ordered if ep.session_key == session_key]
        return ordered[:limit]

    async def _like_search(
        self, query: str, session_key: str | None, limit: int,
    ) -> list[Episode]:
        if session_key:
            rows = await self._storage.fetch_sql(
                "SELECT * FROM memory_episodes WHERE session_key = ? "
                "AND summary LIKE ? ORDER BY created_at DESC LIMIT ?",
                (session_key, f"%{query}%", limit),
            )
        else:
            rows = await self._storage.fetch_sql(
                "SELECT * FROM memory_episodes WHERE summary LIKE ? "
                "ORDER BY created_at DESC LIMIT ?",
                (f"%{query}%", limit),
            )
        return [self._row_to_episode(r) for r in rows]

    async def get_session_episodes(
        self, session_key: str, limit: int = 50,
    ) -> list[Episode]:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM memory_episodes WHERE session_key = ? "
            "ORDER BY created_at DESC LIMIT ?",
            (session_key, limit),
        )
        return [self._row_to_episode(r) for r in rows]

    async def get_recent(self, limit: int = 20) -> list[Episode]:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM memory_episodes ORDER BY created_at DESC LIMIT ?",
            (limit,),
        )
        return [self._row_to_episode(r) for r in rows]

    @staticmethod
    def _row_to_episode(row: dict[str, Any]) -> Episode:
        entities = row.get("entities", "[]")
        if isinstance(entities, str):
            entities = json.loads(entities)
        return Episode(
            id=row["id"],
            session_key=row.get("session_key", ""),
            summary=row.get("summary", ""),
            message_range_start=row.get("message_range_start", 0),
            message_range_end=row.get("message_range_end", 0),
            entity_ids=entities,
            importance=row.get("importance", 0.5),
            created_at=row.get("created_at", ""),
        )


class SemanticManager:
    """Manages distilled facts -- the core persistent memory tier.

    Wraps existing MemoryStore CRUD with tier filtering.
    """

    def __init__(self, store: Any):
        self._store = store

    def get_semantic_entries(
        self,
        mem_type: MemoryType | None = None,
        session_key: str | None = None,
    ) -> list[MemoryEntry]:
        entries = self._store.list_all(mem_type=mem_type, session_key=session_key)
        return [e for e in entries if e.tier == MemoryTier.SEMANTIC]

    async def promote_from_episodic(
        self, episode: Episode, extracted_facts: list[dict[str, Any]],
    ) -> list[MemoryEntry]:
        """Promote extracted facts from an episode to semantic memory."""
        promoted: list[MemoryEntry] = []
        for fact in extracted_facts:
            fact_type = MemoryType(fact.get("type", "environment"))
            entry = MemoryEntry(
                type=fact_type,
                tier=MemoryTier.SEMANTIC,
                key=fact.get("key", ""),
                content=fact.get("content", ""),
                tags=fact.get("tags", []),
                importance=fact.get("importance", 0.5),
                episode_id=episode.id,
                source="consolidated",
                source_session=episode.session_key if fact_type == MemoryType.USER else "",
            )
            try:
                result = self._store.add(entry)
                promoted.append(result)
            except ValueError as e:
                logger.warning(
                    "Failed to promote fact from episode {}: {}", episode.id, e,
                )
        if promoted:
            logger.info("Promoted {} facts from episode {}", len(promoted), episode.id)
        return promoted


class ArchivalManager:
    """Compressed long-term storage for old/low-importance memories.

    When constructed with the authoritative ``MemoryStore``, tier changes and
    deletions go through it (and thus persist to the JSON files the store
    actually loads from). The raw-SQL path is kept only as a fallback for
    callers that have no store — writing the SQLite mirror alone has no effect
    on what the agent remembers, because the store never reads it back.
    """

    def __init__(self, storage: StorageBackend, store: Any = None):
        self._storage = storage
        self._store = store

    async def archive(self, entries: list[MemoryEntry]) -> int:
        """Move entries to archival tier. Returns count archived."""
        count = 0
        for entry in entries:
            if self._store is not None:
                from echo_agent.memory.types import MemoryTier as _Tier
                if self._store.set_tier(entry.id, _Tier.ARCHIVAL):
                    entry.tier = _Tier.ARCHIVAL
                    count += 1
                continue
            entry.tier = MemoryTier.ARCHIVAL
            entry.updated_at = datetime.now().isoformat()
            await self._storage.execute_sql(
                "UPDATE memories SET data = ?, updated_at = ? WHERE id = ?",
                (
                    json.dumps(entry.to_dict(), ensure_ascii=False),
                    entry.updated_at,
                    entry.id,
                ),
            )
            count += 1
        if count:
            logger.info("Archived {} memories", count)
        return count

    async def delete_forgotten(self, entries: list[MemoryEntry]) -> int:
        count = 0
        for entry in entries:
            if self._store is not None:
                # store.delete persists to JSON and cleans mirror + vectors.
                if self._store.delete(entry.id):
                    count += 1
                continue
            await self._storage.execute_sql(
                "DELETE FROM memories WHERE id = ?", (entry.id,),
            )
            if entry.embedding_id:
                await self._storage.execute_sql(
                    "DELETE FROM vectors WHERE id = ?", (entry.embedding_id,),
                )
            count += 1
        if count:
            logger.info("Deleted {} forgotten memories", count)
        return count
