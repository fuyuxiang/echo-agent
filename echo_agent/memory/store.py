"""Memory system — user memory, environment memory, CRUD, retrieval, consolidation.

Two-layer design:
  - User memory: preferences, habits, long-term requirements
  - Environment memory: project background, tool docs, process rules, domain knowledge

The implementation keeps the structured memory model used by Echo Agent, while
using built-in safety properties:
  - per-target file locking to avoid stale concurrent writes
  - atomic file replacement for durable persistence
  - prompt-injection / exfiltration scanning before memory is accepted
  - bounded snapshot rendering for system-prompt injection
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import tempfile
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Iterable

from loguru import logger

from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType, source_priority
from echo_agent.memory.text import cjk_tokens
from echo_agent.memory.forgetting import ForgettingCurve

msvcrt = None
try:
    import fcntl
except ImportError:
    fcntl = None
    try:
        import msvcrt
    except ImportError:
        pass

_STOP_WORDS = frozenset({
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "shall", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "into", "about", "it", "its",
    "this", "that", "and", "or", "but", "not", "no", "if", "so", "than",
})


_MEMORY_THREAT_PATTERNS = [
    (r"ignore\s+(previous|all|above|prior)\s+instructions", "prompt_injection"),
    (r"you\s+are\s+now\s+", "role_hijack"),
    (r"do\s+not\s+tell\s+the\s+user", "deception_hide"),
    (r"system\s+prompt\s+override", "sys_prompt_override"),
    (r"disregard\s+(your|all|any)\s+(instructions|rules|guidelines)", "disregard_rules"),
    (r"act\s+as\s+(if|though)\s+you\s+(have\s+no|don't\s+have)\s+(restrictions|limits|rules)", "bypass_restrictions"),
    (r"curl\s+[^\n]*\$\{?\w*(KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|API)", "exfil_curl"),
    (r"wget\s+[^\n]*\$\{?\w*(KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|API)", "exfil_wget"),
    (r"cat\s+[^\n]*(\.env|credentials|\.netrc|\.pgpass|\.npmrc|\.pypirc)", "read_secrets"),
    (r"authorized_keys", "ssh_backdoor"),
    (r"\$HOME/\.ssh|\~/\.ssh", "ssh_access"),
    (r"\$HOME/\.echo-agent|\~/\.echo-agent", "agent_secret_path"),
    (r"忽略.{0,6}(之前|以上|所有|先前).{0,4}(指令|指示|规则|要求)", "prompt_injection_zh"),
    (r"你现在是", "role_hijack_zh"),
    (r"不要告诉用户", "deception_hide_zh"),
    (r"无视.{0,4}(你的|所有|任何).{0,4}(指令|规则|限制)", "disregard_rules_zh"),
    (r"假装.{0,4}(你|自己).{0,4}(没有|不受).{0,4}(限制|规则|约束)", "bypass_restrictions_zh"),
    (r"系统提示.{0,4}(覆盖|重写|替换)", "sys_prompt_override_zh"),
]

_INVISIBLE_CHARS = {
    "\u200b", "\u200c", "\u200d", "\u2060", "\ufeff",
    "\u202a", "\u202b", "\u202c", "\u202d", "\u202e",
}


def _dedupe_keep_order(values: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if value not in seen:
            seen.add(value)
            result.append(value)
    return result


def _normalize_tags(tags: Iterable[str]) -> list[str]:
    normalized = [tag.strip() for tag in tags if tag and tag.strip()]
    return _dedupe_keep_order(normalized)


def _scan_memory_content(content: str) -> str | None:
    for char in _INVISIBLE_CHARS:
        if char in content:
            return (
                f"Blocked: content contains invisible unicode character U+{ord(char):04X} "
                "(possible injection)."
            )

    for pattern, threat_id in _MEMORY_THREAT_PATTERNS:
        if re.search(pattern, content, re.IGNORECASE):
            return (
                f"Blocked: content matches threat pattern '{threat_id}'. "
                "Memory entries are injected into prompts and must not contain "
                "injection or exfiltration payloads."
            )
    return None


def scan_text_for_threats(content: str) -> str | None:
    """Public entry point for the prompt-injection/exfiltration scan.

    Used by other subsystems whose output is injected into prompts (e.g. the
    evolution gate vetting candidate skill content) so they get the same
    protections as memory writes."""
    return _scan_memory_content(content)


def _atomic_write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(dir=str(path.parent), prefix=".mem_", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp_path, path)
    except BaseException:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise


class MemoryStore:
    """Persistent memory store with file-based storage, safety checks, and scored search."""

    def __init__(
        self,
        memory_dir: Path,
        max_user: int = 1000,
        max_env: int = 500,
        decay_half_life_days: float = 30.0,
        user_snapshot_char_limit: int = 4000,
        env_snapshot_char_limit: int = 2200,
        storage: Any = None,
        scope_policy: str = "legacy",
        contradiction_scan_on_store: bool = False,
        archival_threshold: float = 0.05,
        forget_threshold: float = 0.01,
    ):
        self.memory_dir = memory_dir
        self.memory_dir.mkdir(parents=True, exist_ok=True)
        self._user_file = memory_dir / "user_memory.json"
        self._env_file = memory_dir / "env_memory.json"
        self._history_file = memory_dir / "HISTORY.md"
        self._long_term_file = memory_dir / "MEMORY.md"
        self._max_user = max_user
        self._max_env = max_env
        self._decay_half_life = decay_half_life_days
        self._user_snapshot_char_limit = user_snapshot_char_limit
        self._env_snapshot_char_limit = env_snapshot_char_limit
        self._entries: dict[str, MemoryEntry] = {}
        self._key_index: dict[str, set[str]] = {}  # key -> set of entry IDs for O(1) conflict lookup
        self._storage = storage
        self._scope_policy = scope_policy
        self._pending_storage_tasks: set = set()
        self._dirty_ids: set[str] = set()
        self._failed_sync: set[str] = set()
        self._load()
        self._forgetting = ForgettingCurve(
            base_half_life_days=decay_half_life_days,
            archive_threshold=archival_threshold,
            forget_threshold=forget_threshold,
        )
        self._vector_index = None  # set externally via set_vector_index()
        self._retriever = None  # set externally via set_retriever()
        self._embed_fn = None  # async callable: str -> list[float]
        # (entry_id, text, old_vec_id, content_hash) tuples awaiting embedding
        self._pending_embeds: list[tuple[str, str, str, str]] = []
        # Serializes flush_pending_embeds: multiple triggers (startup backfill +
        # post-reply, both via the background scheduler) can otherwise run
        # concurrently, snapshot the same entry_id, and each add() a vector for
        # it — producing a duplicate vector + an orphan. The lock makes flush
        # single-flight; a second caller waits, then finds the queue drained.
        self._flush_lock = asyncio.Lock()
        self._contradiction_scan_on_store = contradiction_scan_on_store
        self._contradiction_detector = None  # set externally or lazily created
        # Unresolved-contradiction tracking for core-snapshot admission.
        # Source of truth is the SQL memory_contradictions table; this is an
        # in-memory mirror so the synchronous snapshot path needs zero DB calls.
        import threading
        self._unresolved_lock = threading.Lock()
        self._unresolved_pairs: dict[str, tuple[str, str]] = {}  # contradiction_id -> (a, b)
        self._unresolved_refs: dict[str, int] = {}  # memory_id -> refcount

    def has_pending_embeds(self) -> bool:
        """Check if there are pending embeddings to flush."""
        return bool(self._pending_embeds)

    def set_vector_index(self, index):
        self._vector_index = index

    def set_embed_fn(self, fn):
        self._embed_fn = fn

    def set_retriever(self, retriever):
        self._retriever = retriever

    @staticmethod
    def _content_hash(text: str) -> str:
        return hashlib.blake2b(text.encode("utf-8"), digest_size=8).hexdigest()

    def _queue_embed(self, entry: MemoryEntry, old_vec_id: str = "") -> None:
        if not (self._embed_fn and self._vector_index):
            return
        # Circuit open (provider embedding down): stop enqueueing so the pending
        # queue can't grow without bound while every flush spins O(N) over it.
        # The backend is re-decided on restart, not hot-swapped, so there is no
        # in-process recovery to wait for — dropping is the right call.
        if getattr(self._embed_fn, "tripped", False):
            return
        text = f"{entry.key} {entry.content}" if entry.key else entry.content
        self._pending_embeds.append(
            (entry.id, text, old_vec_id, self._content_hash(text))
        )

    async def flush_pending_embeds(self) -> int:
        """Generate embeddings for queued entries and add to vector index. Returns count.

        Only entries that are successfully embedded (or whose source entry has
        since vanished) are removed from the queue; entries that fail to embed
        are left pending so they are retried — either by the DURABLE scheduler
        tier or simply on the next post-reply flush. This is why the queue is
        not cleared up front: a whole-batch failure (e.g. the embedding backend
        is down) must not silently drop the work."""
        if not self._pending_embeds or not self._embed_fn or not self._vector_index:
            return 0
        async with self._flush_lock:
            return await self._flush_pending_embeds_locked()

    async def _flush_pending_embeds_locked(self) -> int:
        # Re-check under the lock: a prior flush may have drained the queue while
        # we waited, and _embed_fn/_vector_index may have been torn down.
        if not self._pending_embeds or not self._embed_fn or not self._vector_index:
            return 0
        # Circuit open: every embed would return [] and process nothing, so a
        # flush is pure O(N) waste. Skip until a restart re-decides the backend.
        if getattr(self._embed_fn, "tripped", False):
            return 0
        batch = list(self._pending_embeds)
        count = 0
        processed: set[tuple[str, str]] = set()
        assigned: dict[MemoryType, dict[str, str]] = {}
        replaced_old: list[str] = []
        for entry_id, text, old_vec_id, content_hash in batch:
            entry = self._entries.get(entry_id)
            if entry is None:
                # Source entry was deleted/forgotten while queued — nothing to
                # embed, so drop this exact item rather than retrying forever.
                processed.add((entry_id, content_hash))
                continue
            try:
                embedding = await self._embed_fn(text)
                if embedding:
                    # The await above yields; update()/merge may have rewritten
                    # this entry and re-queued a fresh item (and _reload_type
                    # swaps the object), so re-fetch and compare hashes. Only
                    # commit the vector if the entry's CURRENT content still
                    # matches what we embedded.
                    entry = self._entries.get(entry_id)
                    if entry is None:
                        processed.add((entry_id, content_hash))
                        continue
                    current = f"{entry.key} {entry.content}" if entry.key else entry.content
                    if self._content_hash(current) != content_hash:
                        # Stale: the entry was rewritten (and re-queued) while we
                        # embedded the old text. Don't attach this old vector to
                        # the new content, and don't remove old_vec_id (no new
                        # vector was committed). Mark THIS (old-hash) item handled
                        # so it drops — the freshly-queued item carries a
                        # different (id, new_hash) and survives for the next flush.
                        processed.add((entry_id, content_hash))
                        continue
                    vec_id = await self._vector_index.add(entry_id, embedding)
                    if vec_id:
                        # add() also yields: update()/merge may have rewritten
                        # (and _reload_type swapped) this entry DURING the add.
                        # Re-verify before committing — otherwise we'd attach
                        # this old-content vector to the now-new entry and leak
                        # the just-added vec_id (the re-queued item's old_vec_id
                        # was captured before this flush, so replaced_old never
                        # reclaims it).
                        entry = self._entries.get(entry_id)
                        current = (
                            f"{entry.key} {entry.content}"
                            if entry is not None and entry.key
                            else (entry.content if entry is not None else None)
                        )
                        if entry is None or self._content_hash(current) != content_hash:
                            # Stale: the vector we just added is an orphan. Remove
                            # it and drop THIS (old-hash) item; the freshly-queued
                            # item carries the new hash and survives for retry.
                            try:
                                await self._vector_index.remove(vec_id)
                            except Exception as e:
                                logger.warning(
                                    "Failed to remove orphan vector {}: {}", vec_id, e
                                )
                            processed.add((entry_id, content_hash))
                            continue
                        entry.embedding_id = vec_id
                        assigned.setdefault(entry.type, {})[entry_id] = vec_id
                        processed.add((entry_id, content_hash))
                        count += 1
                        if old_vec_id and old_vec_id != vec_id:
                            replaced_old.append(old_vec_id)
            except Exception as e:
                logger.debug("Embedding generation failed for {}: {}", entry_id, e)
        # Drop only exactly-handled items (matched by id AND content hash);
        # failures and concurrently-enqueued items (different hash) stay for retry.
        if processed:
            self._pending_embeds = [
                item for item in self._pending_embeds
                if (item[0], item[3]) not in processed
            ]
        # New vector landed — drop the superseded old row so the index and the
        # vectors table don't accumulate orphans on every content update.
        for old_id in replaced_old:
            try:
                await self._vector_index.remove(old_id)
            except Exception as e:
                logger.warning("Failed to remove replaced vector {}: {}", old_id, e)
        # Persist embedding_id assignments — without this, vector cleanup on
        # delete/forget has nothing to key on and the index grows forever.
        for mem_type, ids in assigned.items():
            try:
                with self._file_lock(self._path_for(mem_type)):
                    self._reload_type(mem_type)
                    changed = False
                    for entry_id, vec_id in ids.items():
                        entry = self._entries.get(entry_id)
                        if entry is not None and entry.embedding_id != vec_id:
                            entry.embedding_id = vec_id
                            self._dirty_ids.add(entry_id)
                            changed = True
                    if changed:
                        self._save_type(mem_type)
            except Exception as e:
                logger.warning("Failed to persist embedding ids for {}: {}", mem_type, e)
        if count:
            logger.info("Generated {} embeddings", count)
        return count

    def queue_missing_embeds(self, stale_source_ids: "Iterable[str]" = ()) -> int:
        """Queue entries that lack a vector (embedding_id empty), whose stored
        vector came from a different embedding model (stale), or whose
        embedding_id dangles — no such row in the loaded index, e.g. the
        vectors DB was deleted while the authoritative JSON files survived.
        Called once at startup after the vector index is initialized. Returns
        queued count."""
        if not self._embed_fn or not self._vector_index:
            return 0
        stale = set(stale_source_ids)
        loaded = getattr(self._vector_index, "loaded_vec_ids", None)
        queued_ids = {item[0] for item in self._pending_embeds}
        count = 0
        for entry in self._entries.values():
            if entry.is_superseded or entry.id in queued_ids:
                continue
            dangling = loaded is not None and entry.embedding_id not in loaded
            if not entry.embedding_id or entry.id in stale or dangling:
                self._queue_embed(entry, old_vec_id=entry.embedding_id)
                count += 1
        if count:
            logger.info("Queued {} entries for embedding backfill", count)
        return count

    async def scan_orphan_vectors(self) -> int:
        """Delete vectors-table rows not referenced by any entry's embedding_id.
        Safety net for rows left behind by pre-fix updates/evictions; runs at
        startup BEFORE the index loads so orphans never enter the matrix.
        Memory owns the vectors table (knowledge uses its own sidecar store)."""
        if not self._storage:
            return 0
        valid = {e.embedding_id for e in self._entries.values() if e.embedding_id}
        episode_ids: set[str] = set()
        episodes_ok = True
        try:
            rows_ep = await self._storage.fetch_sql("SELECT id FROM memory_episodes", ())
            episode_ids = {r["id"] for r in rows_ep}
        except Exception as e:
            episodes_ok = False
            logger.warning(
                "Orphan scan: episodes table unavailable, skipping ep: vectors "
                "to avoid deleting live episode vectors: {}", e,
            )
        try:
            rows = await self._storage.load_vectors_all()
        except Exception as e:
            logger.warning("Orphan vector scan failed to load vectors: {}", e)
            return 0
        removed = 0
        for row in rows:
            source_id = row.get("source_id", "") or ""
            if source_id.startswith("ep:"):
                # Only prune ep: vectors when the episodes table is readable;
                # a transient DB error must not wipe every episode vector.
                if not episodes_ok or source_id[3:] in episode_ids:
                    continue  # live (or conservatively kept) episode vector
            elif row["id"] in valid:
                continue
            try:
                await self._storage.delete_vector(row["id"])
                removed += 1
            except Exception as e:
                logger.debug("Failed to delete orphan vector {}: {}", row["id"], e)
        if removed:
            logger.info("Removed {} orphan vector rows", removed)
        return removed

    @property
    def forgetting_curve(self) -> ForgettingCurve:
        return self._forgetting

    @staticmethod
    @contextmanager
    def _file_lock(path: Path):
        """跨平台文件锁（Unix 用 fcntl，Windows 用 msvcrt）。"""
        lock_path = path.with_suffix(path.suffix + ".lock")
        lock_path.parent.mkdir(parents=True, exist_ok=True)

        if fcntl is None and msvcrt is None:
            yield
            return

        if msvcrt and (not lock_path.exists() or lock_path.stat().st_size == 0):
            lock_path.write_text(" ", encoding="utf-8")

        fd = open(lock_path, "r+" if msvcrt else "a+", encoding="utf-8")
        try:
            if fcntl:
                fcntl.flock(fd, fcntl.LOCK_EX)
            else:
                fd.seek(0)
                msvcrt.locking(fd.fileno(), msvcrt.LK_LOCK, 1)
            yield
        finally:
            if fcntl:
                fcntl.flock(fd, fcntl.LOCK_UN)
            elif msvcrt:
                try:
                    fd.seek(0)
                    msvcrt.locking(fd.fileno(), msvcrt.LK_UNLCK, 1)
                except (OSError, IOError):
                    pass
            fd.close()

    def _path_for(self, mem_type: MemoryType) -> Path:
        return self._user_file if mem_type == MemoryType.USER else self._env_file

    def _typed_entries(self, mem_type: MemoryType) -> list[MemoryEntry]:
        return [entry for entry in self._entries.values() if entry.type == mem_type]

    def _index_entry(self, entry: MemoryEntry) -> None:
        if entry.key:
            self._key_index.setdefault(entry.key, set()).add(entry.id)

    def _unindex_entry(self, entry: MemoryEntry) -> None:
        if entry.key:
            ids = self._key_index.get(entry.key)
            if ids:
                ids.discard(entry.id)
                if not ids:
                    del self._key_index[entry.key]

    def _filtered_entries(
        self, mem_type: MemoryType | None = None, session_key: str | None = None,
    ) -> list[MemoryEntry]:
        """按类型和会话可见性过滤记忆条目。"""
        entries = list(self._entries.values())
        if mem_type is not None:
            entries = [entry for entry in entries if entry.type == mem_type]
        if session_key:
            entries = [entry for entry in entries if self._visible_in_session(entry, session_key)]
        return entries

    def is_visible_in_session(self, entry: MemoryEntry, session_key: str) -> bool:
        """Public wrapper so the retriever can apply the store's visibility policy."""
        return self._visible_in_session(entry, session_key)

    def _visible_in_session(self, entry: MemoryEntry, session_key: str | None = None) -> bool:
        if not session_key:
            return True
        if self._scope_policy == "legacy":
            if entry.type == MemoryType.USER:
                return True
            if entry.type == MemoryType.ENVIRONMENT:
                return True
            if "global" in entry.tags:
                return True
            return entry.source_session == session_key
        if "global" in entry.tags:
            return True
        if not entry.source_session:
            return True
        return entry.source_session == session_key

    def _same_scope(self, existing: MemoryEntry, incoming: MemoryEntry) -> bool:
        if existing.type != incoming.type:
            return False
        if self._scope_policy == "legacy":
            if incoming.type == MemoryType.ENVIRONMENT:
                return True
            return existing.source_session == incoming.source_session
        if "global" in incoming.tags or "global" in existing.tags:
            return "global" in incoming.tags and "global" in existing.tags
        return (existing.source_session or "") == (incoming.source_session or "")

    def _load_type_from_disk(self, mem_type: MemoryType) -> list[MemoryEntry]:
        path = self._path_for(mem_type)
        if not path.exists():
            return []
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except Exception as exc:
            # Quarantine instead of returning [] over a live file: the next
            # save would otherwise overwrite the corrupt-but-maybe-recoverable
            # bytes with only the newest entry, silently wiping all memories.
            quarantine = path.with_name(
                f"{path.name}.corrupt-{datetime.now().strftime('%Y%m%d%H%M%S')}"
            )
            try:
                os.replace(path, quarantine)
                logger.error(
                    "Memory file {} is corrupt and was quarantined to {} — "
                    "recover it manually if needed: {}",
                    path, quarantine.name, exc,
                )
            except OSError as move_exc:
                logger.error(
                    "Memory file {} is corrupt and could not be quarantined ({}): {}",
                    path, move_exc, exc,
                )
            return []

        entries: list[MemoryEntry] = []
        seen_ids: set[str] = set()
        for item in raw:
            try:
                entry = MemoryEntry.from_dict(item)
            except Exception as exc:
                logger.warning("Failed to parse memory entry from {}: {}", path, exc)
                continue
            entry.type = mem_type
            if entry.id in seen_ids:
                continue
            seen_ids.add(entry.id)
            entries.append(entry)
        return entries

    def _reload_type(self, mem_type: MemoryType) -> None:
        previous = {entry.id: entry for entry in self._typed_entries(mem_type)}
        for entry_id in list(previous):
            old_entry = self._entries.pop(entry_id, None)
            if old_entry:
                self._unindex_entry(old_entry)
        for entry in self._load_type_from_disk(mem_type):
            prev = previous.get(entry.id)
            # Carry over unsaved spaced-repetition reinforcement (touch()
            # marks entries dirty but reinforcement doesn't save) — otherwise
            # every reload before a save discards access_count/last_accessed,
            # the inputs to the decay half-life.
            if prev is not None and prev.id in self._dirty_ids and prev.access_count > entry.access_count:
                entry.access_count = prev.access_count
                entry.last_accessed = prev.last_accessed or entry.last_accessed
            self._entries[entry.id] = entry
            self._index_entry(entry)

    def _save_type(self, mem_type: MemoryType) -> None:
        """将指定类型的记忆条目原子写入磁盘。"""
        entries = self._typed_entries(mem_type)
        entries.sort(key=lambda entry: (entry.created_at or "", entry.updated_at or "", entry.id))
        payload = [entry.to_dict() for entry in entries]

        _atomic_write_text(
            self._path_for(mem_type),
            json.dumps(payload, ensure_ascii=False, indent=2),
        )

        if self._storage:
            sync_ids = (self._dirty_ids | self._failed_sync) & {e.id for e in entries}
            for entry in entries:
                if entry.id not in sync_ids:
                    continue
                try:
                    coro = self._storage.store_memory(entry.id, entry.to_dict())
                    try:
                        loop = asyncio.get_running_loop()
                    except RuntimeError:
                        loop = None
                    if loop is not None:
                        task = asyncio.ensure_future(coro)
                        task.add_done_callback(
                            lambda t, eid=entry.id: self._on_storage_sync_done(t, eid)
                        )
                        self._pending_storage_tasks.add(task)
                    else:
                        # No running loop: drive the coroutine on a throwaway
                        # loop so synchronous callers still get the side-effect.
                        # asyncio.run() would close the loop and disrupt later
                        # asyncio.Future() construction in the same thread, so
                        # use a fresh event loop manually and leave thread-local
                        # state intact.
                        new_loop = asyncio.new_event_loop()
                        try:
                            new_loop.run_until_complete(coro)
                        finally:
                            new_loop.close()
                    self._failed_sync.discard(entry.id)
                except Exception as e:
                    logger.warning("Failed to sync memory {} to storage: {}", entry.id, e)
                    self._failed_sync.add(entry.id)
        self._dirty_ids -= {e.id for e in entries}

    def _on_storage_sync_done(self, task: asyncio.Task, entry_id: str) -> None:
        self._pending_storage_tasks.discard(task)
        if task.exception():
            logger.warning("Storage sync task failed for {}: {}", entry_id, task.exception())
            self._failed_sync.add(entry_id)

    def _load(self) -> None:
        self._reload_type(MemoryType.USER)
        self._reload_type(MemoryType.ENVIRONMENT)

    def _find_conflict(self, entry: MemoryEntry) -> MemoryEntry | None:
        if not entry.key:
            return None
        candidate_ids = self._key_index.get(entry.key, set())
        for eid in candidate_ids:
            existing = self._entries.get(eid)
            if existing and existing.type == entry.type and self._same_scope(existing, entry):
                return existing
        return None

    def _validate_content(self, content: str, *, field_name: str = "content") -> str:
        normalized = content.strip()
        if not normalized:
            raise ValueError(f"{field_name} cannot be empty")
        scan_error = _scan_memory_content(normalized)
        if scan_error:
            raise ValueError(scan_error)
        return normalized

    def _evict_oldest(self, mem_type: MemoryType) -> None:
        """淘汰有效重要性最低的记忆条目，为新条目腾出空间。"""
        typed = sorted(
            self._typed_entries(mem_type),
            key=lambda entry: (self._forgetting.effective_importance(entry), entry.updated_at or "", entry.id),
        )
        if typed:
            evicted = self._entries.pop(typed[0].id, None)
            if evicted:
                self._unindex_entry(evicted)
                # 与 delete() 路径对齐:清理向量索引与 SQLite 镜像,
                # 否则容量淘汰会在 FAISS 留下孤儿向量、在镜像表留下死行。
                self._cleanup_deleted(evicted)

    def _merge_locked(self, existing_id: str, new_entry: MemoryEntry) -> MemoryEntry:
        existing = self._entries[existing_id]
        # Provenance guard: a lower-provenance write must not silently
        # overwrite higher-provenance content (e.g. model inference vs. what
        # the user explicitly stated). Keep the content, merge the metadata,
        # and leave a suspected_conflict marker for the resolution flow.
        if source_priority(new_entry.source) < source_priority(existing.source):
            merged_tags = _normalize_tags(
                [*existing.tags, *new_entry.tags, self.SUSPECTED_CONFLICT_TAG]
            )
            existing.tags = merged_tags
            existing.importance = max(existing.importance, new_entry.importance)
            self._dirty_ids.add(existing_id)
            logger.info(
                "Provenance guard: kept '{}' content from {} over incoming {}",
                existing.key, existing.source, new_entry.source,
            )
            self._spawn_blocked_contradiction(existing, new_entry)
            return existing
        new_content = self._validate_content(new_entry.content)
        content_changed = new_content != existing.content
        existing.content = new_content
        existing.tags = _normalize_tags([*existing.tags, *new_entry.tags])
        existing.importance = max(existing.importance, new_entry.importance)
        existing.source = new_entry.source
        existing.updated_at = datetime.now().isoformat()
        self._dirty_ids.add(existing_id)
        # Content changed → the stored vector no longer matches. Re-queue an
        # embed keyed on the old vector so flush replaces it (same contract as
        # update()); otherwise the merge path silently leaves the index
        # pointing at the pre-merge text.
        if content_changed:
            self._queue_embed(existing, old_vec_id=existing.embedding_id)
        return existing

    def _spawn_blocked_contradiction(self, existing: MemoryEntry, blocked: MemoryEntry) -> None:
        """Record the guard-blocked content as a Contradiction row so the
        resolution flow can see it (placeholder memory_id_b = blocked:<source>).
        Async fire-and-forget, mirroring _cleanup_deleted's spawn pattern."""
        if not self._storage:
            return
        from echo_agent.memory.types import Contradiction
        c = Contradiction(
            memory_id_a=existing.id,
            memory_id_b=f"blocked:{blocked.source}",
            description=(
                f"Provenance guard blocked lower-priority write on key "
                f"'{existing.key}': {blocked.content}"
            ),
        )

        async def _record() -> None:
            try:
                await self._storage.execute_sql(
                    "INSERT OR REPLACE INTO memory_contradictions"
                    "(id, memory_id_a, memory_id_b, description, resolution, resolved_at, created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (c.id, c.memory_id_a, c.memory_id_b, c.description,
                     None, None, c.created_at),
                )
            except Exception as e:
                logger.warning("Failed to record blocked contradiction: {}", e)

        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = None
        if loop is not None:
            task = asyncio.ensure_future(_record())
            self._pending_storage_tasks.add(task)
            task.add_done_callback(self._pending_storage_tasks.discard)
        else:
            new_loop = asyncio.new_event_loop()
            try:
                new_loop.run_until_complete(_record())
            finally:
                new_loop.close()

    # ── CRUD ─────────────────────────────────────────────────────────────────

    def add(self, entry: MemoryEntry) -> MemoryEntry:
        """添加记忆条目。自动去重、冲突合并、容量淘汰。"""
        entry.content = self._validate_content(entry.content)
        entry.key = entry.key.strip()
        entry.tags = _normalize_tags(entry.tags)

        path = self._path_for(entry.type)
        with self._file_lock(path):
            self._reload_type(entry.type)

            duplicate = next(
                (
                    existing
                    for existing in self._typed_entries(entry.type)
                    if (
                        existing.key == entry.key
                        and existing.content == entry.content
                        and self._same_scope(existing, entry)
                    )
                ),
                None,
            )
            if duplicate:
                return duplicate

            existing = self._find_conflict(entry)
            if existing:
                merged = self._merge_locked(existing.id, entry)
                self._save_type(entry.type)
                return merged

            limit = self._max_user if entry.type == MemoryType.USER else self._max_env
            if len(self._typed_entries(entry.type)) >= limit:
                self._evict_oldest(entry.type)

            self._entries[entry.id] = entry
            self._index_entry(entry)
            self._dirty_ids.add(entry.id)
            self._queue_embed(entry)

            if self._contradiction_scan_on_store:
                self._run_contradiction_scan(entry)

            # Save after the scan so any suspected_conflict tags it sets on
            # sibling entries are persisted in the same write.
            self._save_type(entry.type)

            return entry

    SUSPECTED_CONFLICT_TAG = "suspected_conflict"

    # Core-snapshot admission thresholds (no config; conservative defaults).
    _SNAPSHOT_MIN_EFFECTIVE_IMPORTANCE = 0.15  # below this (after decay) -> not frozen
    _SNAPSHOT_MIN_IMPORTANCE_UNVISITED = 0.4   # access_count==0 USER facts need this raw importance

    def _admit_to_snapshot(self, entry: MemoryEntry) -> bool:
        """Gate for the frozen core snapshot. Hard gate (all types): exclude
        superseded entries (already-adjudicated losers) and entries in an open
        contradiction (not-yet-adjudicated pairs). Soft gate (USER only):
        exclude low-confidence facts. Any error -> admit (degrade safe)."""
        try:
            if entry.is_superseded:
                return False
            if self.is_unresolved(entry.id):
                return False
            if entry.type == MemoryType.USER:
                if self._forgetting.effective_importance(entry) < self._SNAPSHOT_MIN_EFFECTIVE_IMPORTANCE:
                    return False
                if entry.access_count == 0 and entry.importance < self._SNAPSHOT_MIN_IMPORTANCE_UNVISITED:
                    return False
            return True
        except Exception as e:
            logger.debug("Snapshot admission check failed for {}, admitting: {}", entry.id, e)
            return True


    def _run_contradiction_scan(self, entry: MemoryEntry) -> None:
        """Observe-only contradiction scan on store (heuristic + temporal).

        Write-time scan never supersedes: it only flags involved entries with a
        ``suspected_conflict`` tag so the LLM-backed consolidator (the only place
        that should adjudicate semantic conflicts) can prioritize them for review.
        Actual supersession stays with deterministic same-key merge (_merge_locked)
        and the consolidator's check(). See
        docs/superpowers/specs/2026-06-08-write-time-contradiction-scan-design.md.
        """
        if self._contradiction_detector is None:
            from echo_agent.memory.contradiction import ContradictionDetector
            self._contradiction_detector = ContradictionDetector(storage=self._storage)

        candidates = [e for e in self._typed_entries(entry.type) if e.id != entry.id and not e.is_superseded]
        contradictions = self._contradiction_detector.check_lightweight_sync(entry, candidates)
        for c in contradictions:
            # memory_id_a/b are older/newer as computed by the detector; flag both
            # ends that still exist, never delete or supersede.
            for flagged_id in (c.memory_id_a, c.memory_id_b):
                flagged = self._entries.get(flagged_id)
                if flagged is None:
                    continue
                if self.SUSPECTED_CONFLICT_TAG not in flagged.tags:
                    flagged.tags = _normalize_tags([*flagged.tags, self.SUSPECTED_CONFLICT_TAG])
                    self._dirty_ids.add(flagged_id)
            logger.info("Suspected contradiction flagged on store: {}", c.description)

    def update(
        self,
        entry_id: str,
        content: str | None = None,
        tags: list[str] | None = None,
        source: str | None = None,
    ) -> MemoryEntry | None:
        entry = self._entries.get(entry_id)
        if not entry:
            return None

        normalized_content = self._validate_content(content) if content is not None else None
        normalized_tags = _normalize_tags(tags) if tags is not None else None

        path = self._path_for(entry.type)
        with self._file_lock(path):
            self._reload_type(entry.type)
            entry = self._entries.get(entry_id)
            if not entry:
                return None
            if normalized_content is not None:
                entry.content = normalized_content
            if normalized_tags is not None:
                entry.tags = normalized_tags
            if source is not None:
                entry.source = source
            entry.updated_at = datetime.now().isoformat()
            self._dirty_ids.add(entry_id)
            self._save_type(entry.type)
            if normalized_content is not None:
                self._queue_embed(entry, old_vec_id=entry.embedding_id)
            return entry

    def delete(self, entry_id: str) -> bool:
        entry = self._entries.get(entry_id)
        if not entry:
            return False

        path = self._path_for(entry.type)
        with self._file_lock(path):
            self._reload_type(entry.type)
            entry = self._entries.get(entry_id)
            if not entry:
                return False
            self._entries.pop(entry_id, None)
            self._unindex_entry(entry)
            self._save_type(entry.type)
        # The JSON file is authoritative; also clean the SQLite mirror and the
        # vector index so neither accumulates rows for entries that no longer
        # exist (best-effort, outside the file lock).
        self._cleanup_deleted(entry)
        return True

    def _cleanup_deleted(self, entry: MemoryEntry) -> None:
        async def _cleanup() -> None:
            if self._storage:
                try:
                    await self._storage.delete_memory(entry.id)
                except Exception as e:
                    logger.debug("Mirror cleanup failed for {}: {}", entry.id, e)
            if self._vector_index and entry.embedding_id:
                try:
                    await self._vector_index.remove(entry.embedding_id)
                except Exception as e:
                    logger.debug("Vector cleanup failed for {}: {}", entry.id, e)

        if not self._storage and not (self._vector_index and entry.embedding_id):
            return
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = None
        if loop is not None:
            task = asyncio.ensure_future(_cleanup())
            self._pending_storage_tasks.add(task)
            task.add_done_callback(self._pending_storage_tasks.discard)
        else:
            new_loop = asyncio.new_event_loop()
            try:
                new_loop.run_until_complete(_cleanup())
            finally:
                new_loop.close()

    def set_tier(self, entry_id: str, tier: MemoryTier) -> bool:
        """Persist a tier change (e.g. archival) to the authoritative JSON store."""
        entry = self._entries.get(entry_id)
        if not entry:
            return False
        path = self._path_for(entry.type)
        with self._file_lock(path):
            self._reload_type(entry.type)
            entry = self._entries.get(entry_id)
            if not entry:
                return False
            if entry.tier == tier:
                return True
            entry.tier = tier
            entry.updated_at = datetime.now().isoformat()
            self._dirty_ids.add(entry_id)
            self._save_type(entry.type)
            return True

    def mark_contradiction_unresolved(
        self, contradiction_id: str, memory_id_a: str, memory_id_b: str
    ) -> None:
        """Record that a memory pair has an open (unresolved) contradiction so the
        core snapshot can exclude both ends. Idempotent per contradiction_id."""
        with self._unresolved_lock:
            if contradiction_id in self._unresolved_pairs:
                return
            self._unresolved_pairs[contradiction_id] = (memory_id_a, memory_id_b)
            for mid in (memory_id_a, memory_id_b):
                self._unresolved_refs[mid] = self._unresolved_refs.get(mid, 0) + 1

    def clear_contradiction(self, contradiction_id: str) -> None:
        """Drop tracking for a resolved/removed contradiction. No-op if unknown."""
        with self._unresolved_lock:
            pair = self._unresolved_pairs.pop(contradiction_id, None)
            if pair is None:
                return
            for mid in pair:
                remaining = self._unresolved_refs.get(mid, 0) - 1
                if remaining > 0:
                    self._unresolved_refs[mid] = remaining
                else:
                    self._unresolved_refs.pop(mid, None)

    def is_unresolved(self, memory_id: str) -> bool:
        """True if memory_id is involved in any open contradiction."""
        with self._unresolved_lock:
            return self._unresolved_refs.get(memory_id, 0) > 0

    def reset_unresolved(self) -> None:
        """Clear all tracking (used before a startup rebuild)."""
        with self._unresolved_lock:
            self._unresolved_pairs.clear()
            self._unresolved_refs.clear()

    def mark_superseded(self, entry_id: str, superseded_by: str) -> bool:
        """Persist a supersession marker to the authoritative JSON store, so
        retrieval's ``is_superseded`` filter actually takes effect."""
        entry = self._entries.get(entry_id)
        if not entry:
            return False
        path = self._path_for(entry.type)
        with self._file_lock(path):
            self._reload_type(entry.type)
            entry = self._entries.get(entry_id)
            if not entry:
                return False
            entry.superseded_by = superseded_by
            entry.updated_at = datetime.now().isoformat()
            self._dirty_ids.add(entry_id)
            self._save_type(entry.type)
            return True

    def set_version(self, entry_id: str, version: int) -> bool:
        entry = self._entries.get(entry_id)
        if not entry:
            return False
        path = self._path_for(entry.type)
        with self._file_lock(path):
            self._reload_type(entry.type)
            entry = self._entries.get(entry_id)
            if not entry:
                return False
            entry.version = version
            entry.updated_at = datetime.now().isoformat()
            self._dirty_ids.add(entry_id)
            self._save_type(entry.type)
            return True

    def get(self, entry_id: str) -> MemoryEntry | None:
        return self._entries.get(entry_id)

    def list_all(self, mem_type: MemoryType | None = None, session_key: str | None = None) -> list[MemoryEntry]:
        entries = self._filtered_entries(mem_type, session_key)
        return sorted(entries, key=lambda entry: entry.updated_at or "", reverse=True)

    # ── Search ───────────────────────────────────────────────────────────────

    def search_keyword(
        self,
        query: str,
        mem_type: MemoryType | None = None,
        limit: int = 20,
        session_key: str | None = None,
    ) -> list[MemoryEntry]:
        pattern = re.compile(re.escape(query), re.IGNORECASE)
        results: list[MemoryEntry] = []
        for entry in self._filtered_entries(mem_type, session_key):
            matched = (
                pattern.search(entry.content)
                or pattern.search(entry.key)
                or any(pattern.search(tag) for tag in entry.tags)
            )
            if matched:
                results.append(entry)
        results.sort(key=lambda entry: self._forgetting.effective_importance(entry), reverse=True)
        return results[:limit]

    def search_scored(
        self,
        query: str,
        mem_type: MemoryType | None = None,
        limit: int = 10,
        session_key: str | None = None,
    ) -> list[tuple[MemoryEntry, float]]:
        """Multi-keyword scored search. Returns (entry, score) pairs sorted by score."""
        words = [
            word.lower() for word in re.findall(r"\w+", query)
            if len(word) > 1 and word.lower() not in _STOP_WORDS
        ]
        # Chinese has no whitespace word boundaries, so \w+ groups a whole run
        # into one token and matching degrades to "the entire phrase must appear
        # verbatim". Add CJK single chars + bigrams so partial overlap scores.
        words.extend(cjk_tokens(query.lower()))
        if not words:
            return [
                (entry, self._forgetting.effective_importance(entry))
                for entry in self.search_keyword(query, mem_type, limit, session_key=session_key)
            ]

        scored: list[tuple[MemoryEntry, float]] = []
        for entry in self._filtered_entries(mem_type, session_key):
            haystack = f"{entry.key} {entry.content} {' '.join(entry.tags)}".lower()
            word_hits = sum(1 for word in words if word in haystack)
            if word_hits == 0:
                continue
            coverage = word_hits / len(words)
            eff_imp = self._forgetting.effective_importance(entry)
            score = coverage * 0.7 + eff_imp * 0.3
            scored.append((entry, score))

        scored.sort(key=lambda item: item[1], reverse=True)
        return scored[:limit]

    def reinforce(self, ids: "Iterable[str]") -> int:
        """Spaced-repetition reinforcement for memories that were actually
        USED (injected into context / returned by the memory tool) — search
        hits alone no longer count. Marks dirty; persisted on the next save.
        Returns the number of entries reinforced."""
        count = 0
        for entry_id in ids:
            entry = self._entries.get(entry_id)
            if entry is None:
                continue
            entry.touch()
            self._dirty_ids.add(entry_id)
            count += 1
        return count

    def find_by_key(
        self,
        key: str,
        mem_type: MemoryType | None = None,
        session_key: str | None = None,
    ) -> MemoryEntry | None:
        normalized_key = key.strip()
        if not normalized_key:
            return None
        for entry in self._filtered_entries(mem_type, session_key):
            if entry.key == normalized_key:
                return entry
        return None

    def find_by_content_matches(
        self,
        substring: str,
        mem_type: MemoryType | None = None,
        limit: int | None = 10,
        session_key: str | None = None,
    ) -> list[MemoryEntry]:
        normalized = substring.strip()
        if not normalized:
            return []
        results: list[MemoryEntry] = []
        for entry in self._filtered_entries(mem_type, session_key):
            if normalized in entry.content or normalized in entry.key:
                results.append(entry)
                if limit is not None and len(results) >= limit:
                    break
        return results

    # ── Context injection ────────────────────────────────────────────────────

    def get_context(
        self,
        mem_type: MemoryType | None = None,
        max_entries: int = 50,
        max_chars: int | None = None,
        session_key: str | None = None,
        overflow_notice: str | None = None,
        admit: "Callable[[MemoryEntry], bool] | None" = None,
        collect_ids: list[str] | None = None,
    ) -> str:
        all_entries = sorted(
            self.list_all(mem_type, session_key=session_key),
            key=lambda entry: self._forgetting.effective_importance(entry),
            reverse=True,
        )
        if admit is not None:
            all_entries = [e for e in all_entries if admit(e)]
        entries = all_entries[:max_entries]
        if not entries:
            return ""

        lines: list[str] = []
        used = 0
        dropped = max(0, len(all_entries) - len(entries))
        for entry in entries:
            tags = f" [{', '.join(entry.tags)}]" if entry.tags else ""
            line = f"- **{entry.key}**{tags}: {entry.content}"
            delta = len(line) + (1 if lines else 0)
            if max_chars is not None and used + delta > max_chars:
                if not lines and max_chars > 3:
                    truncated = line[: max_chars - 3].rstrip()
                    if truncated:
                        lines.append(truncated + "...")
                        if collect_ids is not None:
                            collect_ids.append(entry.id)
                dropped += len(entries) - len(lines)
                break
            lines.append(line)
            if collect_ids is not None:
                collect_ids.append(entry.id)
            used += delta
        # Overflow is a curation signal, not silent loss. Tell the model that
        # more durable facts exist than fit the budget so it can consolidate
        # rather than assume what's shown is everything.
        if dropped > 0 and overflow_notice:
            lines.append(overflow_notice.format(dropped=dropped))
        return "\n".join(lines)

    def get_snapshot_with_ids(
        self, session_key: str | None = None
    ) -> tuple[str, frozenset[str]]:
        """Build the bounded memory snapshot for system-prompt injection, plus the
        set of entry ids that entered it (used to de-dup dynamic recall)."""
        parts: list[str] = []
        collected: list[str] = []
        long_term = self.read_long_term()
        if long_term:
            parts.append(f"## Long-term Memory\n\n{long_term}")

        user_ctx = self.get_context(
            MemoryType.USER,
            max_entries=50,
            max_chars=self._user_snapshot_char_limit,
            session_key=session_key,
            overflow_notice=(
                "- _(+{dropped} more durable facts about the user not shown — "
                "consider consolidating with the memory tool)_"
            ),
            admit=self._admit_to_snapshot,
            collect_ids=collected,
        )
        if user_ctx:
            parts.append(f"## What I Know About You\n\n{user_ctx}")

        env_ctx = self.get_context(
            MemoryType.ENVIRONMENT,
            max_entries=30,
            max_chars=self._env_snapshot_char_limit,
            session_key=session_key,
            admit=self._admit_to_snapshot,
            collect_ids=collected,
        )
        if env_ctx:
            parts.append(f"## Environment Memory\n\n{env_ctx}")

        # Reflection deferred conflicts: one-line nudge (reuses the snapshot
        # channel; no new interruption mechanism).
        try:
            pending = sum(
                1 for e in self._entries.values()
                if "needs_user_confirmation" in e.tags and not e.is_superseded
            )
            if pending:
                parts.append(
                    f"_(There are {pending} memory conflict(s) that need your "
                    "confirmation — use the memory tool's list_contradictions "
                    "to review when convenient.)_"
                )
        except Exception:
            pass

        return "\n\n".join(parts), frozenset(collected)

    def get_snapshot(self, session_key: str | None = None) -> str:
        """Text-only snapshot (back-compat wrapper over get_snapshot_with_ids)."""
        return self.get_snapshot_with_ids(session_key=session_key)[0]

    # ── Long-term memory file (MEMORY.md) ────────────────────────────────────

    def read_long_term(self) -> str:
        if self._long_term_file.exists():
            return self._long_term_file.read_text(encoding="utf-8")
        return ""

    def write_long_term(self, content: str) -> None:
        normalized = content.strip()
        if normalized:
            self._validate_content(normalized, field_name="memory_update")
        with self._file_lock(self._long_term_file):
            _atomic_write_text(self._long_term_file, normalized)

    _MAX_HISTORY_BYTES = 1_000_000

    def append_history(self, entry: str) -> None:
        with self._file_lock(self._history_file):
            self._history_file.parent.mkdir(parents=True, exist_ok=True)
            # Rotate instead of growing forever — HISTORY.md is append-only
            # and would otherwise be an unbounded file.
            try:
                if (
                    self._history_file.exists()
                    and self._history_file.stat().st_size > self._MAX_HISTORY_BYTES
                ):
                    rotated = self._history_file.with_name(
                        f"HISTORY-{datetime.now().strftime('%Y%m%d%H%M%S')}.md"
                    )
                    os.replace(self._history_file, rotated)
                    logger.info("Rotated history file to {}", rotated.name)
            except OSError as e:
                logger.warning("History rotation failed: {}", e)
            with open(self._history_file, "a", encoding="utf-8") as handle:
                handle.write(entry.rstrip() + "\n\n")
