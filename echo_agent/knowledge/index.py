"""Local keyword knowledge index for enterprise/internal-doc retrieval."""

from __future__ import annotations

import asyncio
import hashlib
import json
import math
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
        self._loaded = False
        self._lock = threading.Lock()
        self._needs_rebuild = False
        self._embed_fn: Callable[[str], Awaitable[list[float]]] | None = None
        self._vector_store: Any = None
        self._embed_timeout: float = 1.5
        self._chunk_vectors: dict[str, list[float]] = {}

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

    def rebuild(self) -> dict[str, Any]:
        with self._lock:
            self._chunks = []
            self._df = Counter()
            self.docs_dir.mkdir(parents=True, exist_ok=True)
            files = [
                path for path in self.docs_dir.rglob("*")
                if path.is_file() and path.suffix.lower() in self.allowed_extensions
            ]
            for path in sorted(files):
                self._index_file(path)
            self._recompute_stats()
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
                self._loaded = True
                return
            data = json.loads(self.index_path.read_text(encoding="utf-8"))
            self._chunks = list(data.get("chunks", []))
            if data.get("format") != "echo-agent-knowledge-v2" or any(
                "content_hash" not in c for c in self._chunks
            ):
                self._needs_rebuild = True
            self._recompute_stats()
            self._loaded = True

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
        loop = asyncio.get_running_loop()
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

    async def search_async(self, query: str, *, limit: int = 5, user_id: str = "") -> list[KnowledgeSearchResult]:
        self._ensure_loaded()
        with self._lock:
            chunks_snapshot = list(self._chunks)
            df_snapshot = Counter(self._df)
        kw = self._keyword_scores(query, chunks_snapshot, df_snapshot, user_id=user_id)
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
                       if cid in by_id and self._allowed_for_user(by_id[cid].get("metadata", {}), user_id)}
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
        df: Counter, *, user_id: str,
    ) -> dict[str, float]:
        query_terms = _tokenize(query)
        if not query_terms:
            return {}
        query_counts = Counter(query_terms)
        total_chunks = max(1, len(chunks))
        query_lower = query.lower()
        scores: dict[str, float] = {}
        for chunk in chunks:
            if not self._allowed_for_user(chunk.get("metadata", {}), user_id):
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

    def search(self, query: str, *, limit: int = 5, user_id: str = "") -> list[KnowledgeSearchResult]:
        self._ensure_loaded()
        with self._lock:
            chunks_snapshot = list(self._chunks)
            df_snapshot = Counter(self._df)
        scores = self._keyword_scores(query, chunks_snapshot, df_snapshot, user_id=user_id)
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
        }

    def _ensure_loaded(self) -> None:
        if not self._loaded:
            self.load()

    def _is_stale(self) -> bool:
        if not self.index_path.exists():
            return True
        index_mtime = self.index_path.stat().st_mtime
        if not self.docs_dir.exists():
            return False
        for path in self.docs_dir.rglob("*"):
            if path.is_file() and path.suffix.lower() in self.allowed_extensions and path.stat().st_mtime > index_mtime:
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

    def _allowed_for_user(self, metadata: dict[str, Any], user_id: str) -> bool:
        allowed = metadata.get("allowed_users") or metadata.get("allow_users") or metadata.get("users")
        if not allowed:
            return True
        if isinstance(allowed, str):
            allowed_values = {allowed}
        else:
            allowed_values = {str(value) for value in allowed}
        return "*" in allowed_values or user_id in allowed_values

    def _recompute_stats(self) -> None:
        self._df = Counter()
        for chunk in self._chunks:
            self._df.update(set(chunk.get("terms", {}).keys()))

    def _save(self) -> None:
        self.index_path.parent.mkdir(parents=True, exist_ok=True)
        data = {
            "format": "echo-agent-knowledge-v2",
            "generated_at": datetime.now().isoformat(),
            "docs_dir": str(self.docs_dir),
            "chunks": self._chunks,
        }
        self.index_path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
