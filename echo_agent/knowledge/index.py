"""Local keyword knowledge index for enterprise/internal-doc retrieval."""

from __future__ import annotations

import asyncio
import hashlib
import json
import math
import os
import re
import threading
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Awaitable, Callable

from loguru import logger


_LATIN_OR_NUM_RE = re.compile(r"[a-z0-9_]+", re.I)
_CJK_RE = re.compile(r"[\u4e00-\u9fff]")

# Fusion baseline mirrors echo_agent/memory/retrieval.py resonance weights.
# Kept as a module constant (not config) until tuning data justifies a knob.
_FUSION_BASE = 0.5

#: On-disk index format. v3 adds the file manifest that makes deletions and
#: renames detectable; a v2 index still loads but is rebuilt once to gain one.
_INDEX_FORMAT = "echo-agent-knowledge-v3"


@dataclass
class KnowledgeSearchResult:
    citation_id: str
    path: str
    title: str
    text: str
    score: float
    chunk_id: str
    metadata: dict[str, Any] = field(default_factory=dict)


def _resolve_path(workspace: Path, value: str) -> Path:
    path = Path(value).expanduser()
    return path if path.is_absolute() else workspace / path


def _tokenize(text: str) -> list[str]:
    lower = text.lower()
    tokens = _LATIN_OR_NUM_RE.findall(lower)
    cjk_chars = _CJK_RE.findall(lower)
    tokens.extend(cjk_chars)
    tokens.extend("".join(cjk_chars[i:i + 2]) for i in range(max(0, len(cjk_chars) - 1)))
    return tokens


def _extract_frontmatter(text: str) -> tuple[dict[str, Any], str]:
    if not text.startswith("---\n"):
        return {}, text
    end = text.find("\n---", 4)
    if end < 0:
        return {}, text
    raw = text[4:end].strip()
    body = text[end + 4:].lstrip()
    metadata: dict[str, Any] = {}
    current_key = ""
    for line in raw.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("- ") and current_key:
            metadata.setdefault(current_key, []).append(stripped[2:].strip().strip("'\""))
            continue
        if ":" not in stripped:
            continue
        key, value = stripped.split(":", 1)
        current_key = key.strip()
        value = value.strip()
        if not value:
            metadata[current_key] = []
        elif value.startswith("[") and value.endswith("]"):
            metadata[current_key] = [v.strip().strip("'\"") for v in value[1:-1].split(",") if v.strip()]
        else:
            metadata[current_key] = value.strip("'\"")
    return metadata, body


class KnowledgeIndex:
    """Persistent local index with deterministic keyword ranking and citations."""

    def __init__(
        self,
        *,
        workspace: Path,
        docs_dir: str,
        index_path: str,
        chunk_size: int = 1200,
        chunk_overlap: int = 120,
        allowed_extensions: list[str] | None = None,
    ):
        self.workspace = workspace
        self.docs_dir = _resolve_path(workspace, docs_dir)
        self.index_path = _resolve_path(workspace, index_path)
        self.chunk_size = max(200, chunk_size)
        self.chunk_overlap = max(0, min(chunk_overlap, self.chunk_size // 2))
        self.allowed_extensions = {ext.lower() for ext in (allowed_extensions or [".md", ".txt"])}
        self._chunks: list[dict[str, Any]] = []
        self._df: Counter[str] = Counter()
        # Corpus snapshot (path → [mtime, size]) persisted with the index. This
        # is what makes deletions and renames visible to _is_stale; see
        # _build_manifest for why an mtime comparison alone cannot see them.
        self._manifest: dict[str, list[float]] = {}
        self._loaded = False
        self._lock = threading.Lock()
        self._needs_rebuild = False
        self._embed_fn: Callable[[str], Awaitable[list[float]]] | None = None
        self._vector_store: Any = None
        self._embed_timeout: float = 1.5
        self._chunk_vectors: dict[str, list[float]] = {}
        # Single-flight guard for ``rebuild_async``. The body of rebuild_async
        # crosses ``await`` points (executor + per-chunk embedding), so the
        # plain ``threading.Lock`` from earlier only protected the synchronous
        # halves and let two callers race on the same sidecar — A's sidecar
        # write could clobber B's. ``asyncio.Lock`` serializes the whole
        # rebuild; the *future* is cached so concurrent callers join it
        # rather than queueing, which would otherwise let a user click
        # "rebuild" twice and watch two backfills run sequentially.
        self._rebuild_lock = asyncio.Lock()
        self._rebuild_future: asyncio.Future[dict[str, Any]] | None = None

    @property
    def chunk_count(self) -> int:
        self._ensure_loaded()
        return len(self._chunks)

    @property
    def doc_count(self) -> int:
        self._ensure_loaded()
        return len({chunk["path"] for chunk in self._chunks})

    def ensure_ready(self, *, auto_index: bool = True) -> None:
        if self.index_path.exists() and not self._is_stale():
            self.load()
            if self._needs_rebuild and auto_index:
                self.rebuild()
            return
        if auto_index:
            self.rebuild()
        elif self.index_path.exists():
            self.load()

    def _scan_docs(self) -> list[Path]:
        """Indexable files currently on disk, sorted for deterministic output."""
        if not self.docs_dir.exists():
            return []
        return sorted(
            path for path in self.docs_dir.rglob("*")
            if path.is_file() and path.suffix.lower() in self.allowed_extensions
        )

    def _manifest_key(self, path: Path) -> str:
        """Stable identity for a source file: the path recorded on its chunks."""
        if path.is_relative_to(self.workspace):
            return str(path.relative_to(self.workspace))
        return str(path)

    def _build_manifest(self, files: list[Path]) -> dict[str, list[float]]:
        """Snapshot of the corpus: path → [mtime, size].

        Recorded so staleness can be decided by *comparing sets* rather than by
        asking "is any file newer than the index". The latter cannot see a
        deletion or a rename at all — the surviving files are all older than the
        index, so the index looked fresh while still serving chunks of documents
        that no longer exist.
        """
        manifest: dict[str, list[float]] = {}
        for path in files:
            try:
                stat = path.stat()
            except OSError:
                continue
            manifest[self._manifest_key(path)] = [stat.st_mtime, float(stat.st_size)]
        return manifest

    def rebuild(self) -> dict[str, Any]:
        with self._lock:
            self._chunks = []
            self._df = Counter()
            self.docs_dir.mkdir(parents=True, exist_ok=True)
            files = self._scan_docs()
            for path in files:
                self._index_file(path)
            self._recompute_stats()
            self._manifest = self._build_manifest(files)
            self._save()
            self._loaded = True
            self._needs_rebuild = False
        summary = {"documents": len(files), "chunks": len(self._chunks), "index_path": str(self.index_path)}
        logger.info("Knowledge index rebuilt: {} documents, {} chunks", summary["documents"], summary["chunks"])
        return summary

    def load(self) -> None:
        with self._lock:
            self._needs_rebuild = False
            if not self.index_path.exists():
                self._chunks = []
                self._df = Counter()
                self._manifest = {}
                self._loaded = True
                return
            try:
                data = json.loads(self.index_path.read_text(encoding="utf-8"))
                if not isinstance(data, dict):
                    raise ValueError("index root is not an object")
                chunks = list(data.get("chunks", []))
            except (json.JSONDecodeError, ValueError, OSError, UnicodeDecodeError) as e:
                # A half-written or truncated index (power loss mid-save, full
                # disk) used to raise straight out of here. ensure_ready() is
                # called from AgentLoop.__init__, so that single corrupt file
                # stopped the whole agent from starting, with a JSONDecodeError
                # that named neither the file nor the cause. Quarantine it and
                # rebuild from the source documents, which are the real
                # authority — the index is a derived artifact.
                self._quarantine_index(e)
                self._chunks = []
                self._df = Counter()
                self._manifest = {}
                self._needs_rebuild = True
                self._loaded = True
                return
            self._chunks = chunks
            raw_manifest = data.get("manifest")
            self._manifest = raw_manifest if isinstance(raw_manifest, dict) else {}
            if data.get("format") != _INDEX_FORMAT or any(
                "content_hash" not in c for c in self._chunks
            ):
                self._needs_rebuild = True
            # An index written before manifests existed has none. Treat that as
            # needing a rebuild rather than as "empty corpus", which would read
            # as "every document was deleted" on the next staleness check.
            if not self._manifest and self._chunks:
                self._needs_rebuild = True
            self._recompute_stats()
            self._loaded = True

    def _quarantine_index(self, reason: Exception) -> None:
        """Move an unreadable index aside so the rebuild has a clean target.

        Renamed rather than deleted: it is the only evidence of what went wrong,
        and it may still be useful for diagnosis. Failure to move it is not
        fatal — _save() will overwrite it anyway.
        """
        stamp = datetime.now().strftime("%Y%m%d%H%M%S")
        broken = self.index_path.with_name(f"{self.index_path.name}.corrupt.{stamp}")
        try:
            self.index_path.replace(broken)
            logger.error(
                "Knowledge index at {} is unreadable ({}); quarantined to {} and "
                "rebuilding from source documents.",
                self.index_path, reason, broken.name,
            )
        except OSError as e:
            logger.error(
                "Knowledge index at {} is unreadable ({}) and could not be "
                "quarantined ({}); rebuilding over it.",
                self.index_path, reason, e,
            )

    def attach_embedding(
        self, embed_fn: Callable[[str], Awaitable[list[float]]],
        dimensions: int, *, embed_timeout: float = 1.5,
    ) -> None:
        """Wire an async embedding fn + sidecar vector store. Returns immediately;
        does NOT compute embeddings (that happens in rebuild_async, backgrounded)."""
        from echo_agent.knowledge.vector_store import KnowledgeVectorStore
        self._embed_fn = embed_fn
        self._embed_timeout = max(0.1, float(embed_timeout))
        sidecar = self.index_path.with_name(self.index_path.name + ".vectors.npz")
        self._vector_store = KnowledgeVectorStore(sidecar, dimensions=dimensions)
        self._ensure_loaded()
        self._chunk_vectors = self._vector_store.load()
        ordered = [(c["id"], self._chunk_vectors[c["id"]])
                   for c in self._chunks if c["id"] in self._chunk_vectors]
        self._vector_store.build(ordered)

    def needs_vector_backfill(self) -> bool:
        if not self._vector_store or not self._vector_store.available or not self._embed_fn:
            return False
        self._ensure_loaded()
        # 陈旧判定靠 sidecar 自记录的 content_hash:重启启动期 ensure_ready 已先把
        # 文本索引 rebuild 成新内容(chunk id 不变但 content_hash 变),仅比对 id 命中
        # 会漏判,导致语义检索一直吃旧向量。故 id 缺失或 hash 不一致都需补建。
        sidecar_hashes = self._vector_store.content_hashes()
        for c in self._chunks:
            cid = c["id"]
            if cid not in self._chunk_vectors:
                return True
            if sidecar_hashes.get(cid) != c.get("content_hash"):
                return True
        return False

    async def rebuild_async(self) -> dict[str, Any]:
        # Single-flight: if a rebuild is already running, return its result
        # when it completes rather than starting a parallel one. Two
        # concurrent rebuilds racing on the vector sidecar was the data-
        # integrity bug this protects (reviewer P2).
        if self._rebuild_future is not None and not self._rebuild_future.done():
            return await self._rebuild_future
        loop = asyncio.get_running_loop()
        future: asyncio.Future[dict[str, Any]] = loop.create_future()
        self._rebuild_future = future
        try:
            result = await self._run_rebuild(loop)
        except BaseException as exc:
            future.set_exception(exc)
            # The owner re-raises directly instead of awaiting ``future``.
            # Mark the exception as retrieved so a rebuild with no concurrent
            # waiter does not leak "Future exception was never retrieved" at
            # event-loop shutdown. Any waiter already awaiting the same future
            # still receives the original exception.
            future.exception()
            raise
        else:
            future.set_result(result)
            return result
        finally:
            # Only clear if we're still the owner — another rebuild that
            # started under the lock should keep its own future live.
            if self._rebuild_future is future:
                self._rebuild_future = None

    async def _run_rebuild(self, loop: asyncio.AbstractEventLoop) -> dict[str, Any]:
        async with self._rebuild_lock:
            if not self._vector_store or not self._vector_store.available or not self._embed_fn:
                return await loop.run_in_executor(None, self.rebuild)
            # 0) 复用判定的权威依据是 sidecar 自记录的 content_hash,而非 self._chunks 的
            #    旧 hash:重启启动期 ensure_ready 可能已把 self._chunks rebuild 成新内容,
            #    此时旧 hash 已失真,只有 sidecar 知道每个向量是基于哪段文本算出来的。
            self._ensure_loaded()
            old_vectors = self._vector_store.load()
            sidecar_hashes = self._vector_store.content_hashes()
            # 1) 重建文本索引(同步内核,executor 防阻塞事件循环)
            summary = await loop.run_in_executor(None, self.rebuild)
            # 2) 仅对 id 命中且 sidecar 记录的 hash 与当前 chunk hash 一致的复用,余者重算
            new_vectors: dict[str, list[float]] = {}
            for chunk in self._chunks:
                cid = chunk["id"]
                reuse = old_vectors.get(cid)
                if reuse is not None and sidecar_hashes.get(cid) == chunk["content_hash"]:
                    new_vectors[cid] = reuse
                    continue
                try:
                    emb = await asyncio.wait_for(self._embed_fn(chunk["text"]), self._embed_timeout)
                except Exception as e:
                    logger.warning("knowledge embed failed for {}: {}", cid, e)
                    continue
                if emb:
                    new_vectors[cid] = emb
            ordered = [(c["id"], new_vectors[c["id"]]) for c in self._chunks if c["id"] in new_vectors]
            new_hashes = {c["id"]: c["content_hash"] for c in self._chunks if c["id"] in new_vectors}
            self._vector_store.build(ordered)
            self._vector_store.save(ordered, new_hashes)
            self._chunk_vectors = new_vectors
            logger.info("knowledge vectors backfilled: {} chunks", len(ordered))
            return summary

    async def search_async(self, query: str, *, limit: int = 5, user_id: str = "", channel: str = "") -> list[KnowledgeSearchResult]:
        self._ensure_loaded()
        with self._lock:
            chunks_snapshot = list(self._chunks)
            df_snapshot = Counter(self._df)
        kw = self._keyword_scores(query, chunks_snapshot, df_snapshot, user_id=user_id, channel=channel)
        vec: dict[str, float] = {}
        if self._vector_store and self._vector_store.available and self._embed_fn:
            try:
                q_emb = await asyncio.wait_for(self._embed_fn(query), self._embed_timeout)
                if q_emb:
                    for cid, score in self._vector_store.search(q_emb, limit * 3):
                        vec[cid] = score
            except Exception:
                logger.debug("knowledge query embed failed; keyword-only")
        by_id = {c["id"]: c for c in chunks_snapshot}
        allowed_vec = {cid: s for cid, s in vec.items()
                       if cid in by_id and self._allowed_for_user(by_id[cid].get("metadata", {}), user_id, channel=channel)}
        fused = self._fuse(query, kw, allowed_vec)
        ranked = sorted(fused.items(), key=lambda kv: kv[1], reverse=True)[:limit]
        return self._to_results([(by_id[cid], sc) for cid, sc in ranked if cid in by_id])

    def _fuse(self, query: str, kw: dict[str, float], vec: dict[str, float]) -> dict[str, float]:
        if not vec:
            return kw  # 降级:等价同步 search 的纯关键词
        kw_max = max(kw.values(), default=1.0) or 1.0
        vec_max = max(vec.values(), default=1.0) or 1.0
        e = self._query_entropy(query)
        w_kw = _FUSION_BASE * (1 - e)
        w_vec = _FUSION_BASE + _FUSION_BASE * e
        fused: dict[str, float] = {}
        for cid in set(kw) | set(vec):
            fused[cid] = w_kw * (kw.get(cid, 0.0) / kw_max) + w_vec * (vec.get(cid, 0.0) / vec_max)
        return fused

    @staticmethod
    def _query_entropy(query: str) -> float:
        tokens = _tokenize(query)
        if not tokens:
            return 0.5
        counts = Counter(tokens)
        total = len(tokens)
        entropy = -sum((c / total) * math.log2(c / total) for c in counts.values())
        max_ent = math.log2(len(counts)) if len(counts) > 1 else 1.0
        return min(entropy / max_ent, 1.0) if max_ent > 0 else 0.0

    def _keyword_scores(
        self, query: str, chunks: list[dict[str, Any]],
        df: Counter, *, user_id: str, channel: str = "",
    ) -> dict[str, float]:
        query_terms = _tokenize(query)
        if not query_terms:
            return {}
        query_counts = Counter(query_terms)
        total_chunks = max(1, len(chunks))
        query_lower = query.lower()
        scores: dict[str, float] = {}
        for chunk in chunks:
            if not self._allowed_for_user(chunk.get("metadata", {}), user_id, channel=channel):
                continue
            terms = Counter(chunk.get("terms", {}))
            if not terms:
                continue
            score = 0.0
            length = max(1, sum(terms.values()))
            for term, query_tf in query_counts.items():
                tf = terms.get(term, 0)
                if tf <= 0:
                    continue
                idf = math.log((1 + total_chunks) / (1 + df.get(term, 0))) + 1
                score += (tf / length) * idf * query_tf
            text_lower = chunk.get("text", "").lower()
            if query_lower and query_lower in text_lower:
                score += 1.5
            if score > 0:
                scores[chunk["id"]] = score
        return scores

    def _to_results(self, scored: list[tuple[dict[str, Any], float]]) -> list[KnowledgeSearchResult]:
        results: list[KnowledgeSearchResult] = []
        for idx, (chunk, score) in enumerate(scored, 1):
            results.append(KnowledgeSearchResult(
                citation_id=f"K{idx}",
                path=chunk.get("path", ""),
                title=chunk.get("title", "") or Path(chunk.get("path", "")).name,
                text=chunk.get("text", ""),
                score=round(score, 6),
                chunk_id=chunk.get("id", ""),
                metadata=chunk.get("metadata", {}),
            ))
        return results

    def search(self, query: str, *, limit: int = 5, user_id: str = "", channel: str = "") -> list[KnowledgeSearchResult]:
        self._ensure_loaded()
        with self._lock:
            chunks_snapshot = list(self._chunks)
            df_snapshot = Counter(self._df)
        scores = self._keyword_scores(query, chunks_snapshot, df_snapshot, user_id=user_id, channel=channel)
        by_id = {c["id"]: c for c in chunks_snapshot}
        ranked = sorted(scores.items(), key=lambda kv: kv[1], reverse=True)[:limit]
        return self._to_results([(by_id[cid], sc) for cid, sc in ranked])

    def format_results(self, results: list[KnowledgeSearchResult]) -> str:
        if not results:
            return ""
        lines = [
            "Internal knowledge context. Use these citations when answering claims sourced from internal docs."
        ]
        for result in results:
            excerpt = re.sub(r"\s+", " ", result.text).strip()
            if len(excerpt) > 900:
                excerpt = excerpt[:900] + "..."
            lines.append(
                f"[{result.citation_id}] {result.title} ({result.path})\n"
                f"{excerpt}"
            )
        return "\n\n".join(lines)

    def status(self) -> dict[str, Any]:
        self._ensure_loaded()
        return {
            "docs_dir": str(self.docs_dir),
            "index_path": str(self.index_path),
            "documents": self.doc_count,
            "chunks": self.chunk_count,
            "allowed_extensions": sorted(self.allowed_extensions),
            "stale": self._is_stale(),
            "last_rebuild": self._last_rebuild_iso(),
        }

    def _last_rebuild_iso(self) -> str | None:
        """When the index file was last written, as a local-time ISO string.

        The index has no build timestamp of its own; ``_save`` rewrites the file
        on every rebuild, so its mtime is exactly the last successful rebuild.
        None means the index was never built.
        """
        if not self.index_path.exists():
            return None
        return datetime.fromtimestamp(self.index_path.stat().st_mtime).isoformat(timespec="seconds")

    def _ensure_loaded(self) -> None:
        if not self._loaded:
            self.load()

    def _is_stale(self) -> bool:
        """Whether the index no longer matches the documents on disk.

        Compares the recorded manifest against a fresh scan, so additions,
        **deletions**, renames and in-place edits are all detected. The previous
        "is any file newer than the index file" test could only ever see the
        first and last of those: after a delete, every remaining file was older
        than the index, so it reported fresh and kept serving chunks from the
        removed document (a ghost hit that survived restarts).
        """
        if not self.index_path.exists():
            return True
        current = self._build_manifest(self._scan_docs())
        recorded = self._recorded_manifest()
        if recorded is None:
            # Pre-manifest index: fall back to the old mtime heuristic rather
            # than forcing a rebuild on every startup for existing installs.
            return self._is_stale_by_mtime()
        if set(current) != set(recorded):
            return True
        for key, (mtime, size) in current.items():
            prev = recorded.get(key) or []
            if len(prev) < 2 or mtime > prev[0] or size != prev[1]:
                return True
        return False

    def _recorded_manifest(self) -> dict[str, list[float]] | None:
        """The manifest stored in the index file, or None if it predates them.

        Read from disk rather than from ``self._manifest``: staleness is checked
        before ``load()`` in ``ensure_ready``, so the in-memory copy is empty at
        that point and would read as "the corpus was emptied".
        """
        if self._manifest:
            return self._manifest
        try:
            data = json.loads(self.index_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, ValueError, OSError, UnicodeDecodeError):
            # Unreadable: let load() quarantine and rebuild rather than deciding
            # freshness from a file we cannot parse.
            return {}
        if not isinstance(data, dict):
            return {}
        manifest = data.get("manifest")
        if isinstance(manifest, dict):
            return manifest
        return None

    def _is_stale_by_mtime(self) -> bool:
        index_mtime = self.index_path.stat().st_mtime
        for path in self._scan_docs():
            if path.stat().st_mtime > index_mtime:
                return True
        return False

    def _index_file(self, path: Path) -> None:
        from echo_agent.knowledge.extractors import extract_text

        raw = extract_text(path)
        if raw is None:
            return
        metadata, body = _extract_frontmatter(raw)
        rel_path = str(path.relative_to(self.workspace)) if path.is_relative_to(self.workspace) else str(path)
        title = self._title_for(path, body)
        for idx, text in enumerate(self._chunk_text(body)):
            terms = Counter(_tokenize(text))
            self._chunks.append({
                "id": f"{rel_path}#{idx}",
                "path": rel_path,
                "title": title,
                "text": text,
                "terms": dict(terms),
                "content_hash": hashlib.sha1(text.encode("utf-8")).hexdigest(),
                "metadata": metadata,
                "mtime": path.stat().st_mtime,
            })

    def _chunk_text(self, text: str) -> list[str]:
        clean = text.strip()
        if not clean:
            return []
        if len(clean) <= self.chunk_size:
            return [clean]
        chunks: list[str] = []
        step = self.chunk_size - self.chunk_overlap
        start = 0
        while start < len(clean):
            end = min(len(clean), start + self.chunk_size)
            chunks.append(clean[start:end].strip())
            if end >= len(clean):
                break
            start += step
        return [c for c in chunks if c]

    def _title_for(self, path: Path, text: str) -> str:
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("#"):
                return stripped.lstrip("#").strip() or path.name
            if stripped:
                return stripped[:80]
        return path.name

    def _allowed_for_user(self, metadata: dict[str, Any], user_id: str, *, channel: str = "") -> bool:
        allowed = metadata.get("allowed_users") or metadata.get("allow_users") or metadata.get("users")
        if not allowed:
            return True
        if isinstance(allowed, str):
            allowed_values = {allowed}
        else:
            allowed_values = {str(value) for value in allowed}
        if "*" in allowed_values:
            return True
        # Match both namespaced ("telegram:12345") and bare ("12345") forms so
        # that documents configured with either style work across channels.
        if user_id in allowed_values:
            return True
        if channel and f"{channel}:{user_id}" in allowed_values:
            return True
        # Also check if the stored value is namespaced and the incoming user_id
        # matches the bare part (e.g. stored "telegram:12345", incoming "12345"
        # from telegram).
        if channel:
            for v in allowed_values:
                if ":" in v and v.split(":", 1)[0] == channel and v.split(":", 1)[1] == user_id:
                    return True
        return False

    def _recompute_stats(self) -> None:
        self._df = Counter()
        for chunk in self._chunks:
            self._df.update(set(chunk.get("terms", {}).keys()))

    def _save(self) -> None:
        """Write the index atomically.

        temp + fsync + os.replace, because a plain ``write_text`` leaves a
        truncated file behind if the process dies (or the disk fills) mid-write,
        and that half-file is what the next startup tries to parse. os.replace is
        atomic within a filesystem, so a reader sees either the old index or the
        new one, never a partial one.
        """
        self.index_path.parent.mkdir(parents=True, exist_ok=True)
        data = {
            "format": _INDEX_FORMAT,
            "generated_at": datetime.now().isoformat(),
            "docs_dir": str(self.docs_dir),
            "manifest": self._manifest,
            "chunks": self._chunks,
        }
        tmp = self.index_path.with_name(f".{self.index_path.name}.tmp")
        try:
            with tmp.open("w", encoding="utf-8") as fh:
                json.dump(data, fh, ensure_ascii=False, indent=2)
                fh.flush()
                os.fsync(fh.fileno())
            tmp.replace(self.index_path)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
