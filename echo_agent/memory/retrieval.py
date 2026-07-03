"""Hybrid retrieval — BM25 + vector + RRF (Reciprocal Rank Fusion)."""

from __future__ import annotations

import asyncio
import math
from collections import Counter
from dataclasses import dataclass
from typing import Callable, Awaitable, TYPE_CHECKING

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.memory.vectors import VectorIndex

from echo_agent.memory.types import MemoryEntry, MemoryType, Episode
from echo_agent.memory.forgetting import ForgettingCurve
from echo_agent.memory.text import tokenize as _tokenize_shared

_RRF_K = 60


@dataclass
class _EpisodicProxy:
    """Lightweight wrapper so Episodes participate in BM25 scoring transparently."""
    id: str
    key: str
    content: str
    tags: list
    is_superseded: bool = False
    type: MemoryType = MemoryType.ENVIRONMENT
    importance: float = 0.5
    access_count: int = 0
    last_accessed: str = ""
    _episode: Episode | None = None


class HybridRetriever:
    """Multi-signal retrieval with Reciprocal Rank Fusion (RRF).

    Fuses BM25 keyword ranking and vector similarity ranking using RRF,
    then applies forgetting-curve importance decay.
    """

    def __init__(
        self,
        entries_fn: Callable[[], list[MemoryEntry]],
        vector_index: VectorIndex | None = None,
        forgetting: ForgettingCurve | None = None,
        embed_fn: Callable[[str], Awaitable[list[float]]] | None = None,
        embed_timeout: float = 1.5,
        visibility_fn: Callable[["MemoryEntry", str], bool] | None = None,
        episode_search_fn: Callable[[str, str, int], Awaitable[list[Episode]]] | None = None,
        episode_candidate_limit: int = 10,
    ):
        self._entries_fn = entries_fn
        self._vector_index = vector_index
        self._forgetting = forgetting or ForgettingCurve()
        self._embed_fn = embed_fn
        self._visibility_fn = visibility_fn
        # Assembles episodic candidates by relevance (semantic + LIKE fallback)
        # when the caller does not pass `episodes` explicitly. Injecting it here
        # is what lets episodes flow through the SAME retrieve() call the
        # prefetcher warms — so a cache hit already carries episodes and a
        # latency-first CLI turn (degrade-on-miss) still recalls them, instead
        # of episodes only appearing on the inline sync-miss path.
        self._episode_search_fn = episode_search_fn
        self._episode_candidate_limit = max(1, int(episode_candidate_limit))
        # Latency budget for the embedding round-trip (configurable via
        # memory.embedTimeoutSeconds). Retrieval runs on the user-facing
        # critical path of every message; vector similarity is an enhancement,
        # so a slow embedding endpoint degrades to BM25-only for that turn
        # rather than stalling the reply.
        self._embed_timeout = max(0.1, float(embed_timeout))
        self._embed_timeout_warned = False

    async def retrieve(
        self, query: str, limit: int = 8,
        session_key: str = "", mem_type: MemoryType | None = None,
        episodes: list[Episode] | None = None,
    ) -> list[tuple[MemoryEntry | Episode, float]]:
        """混合检索管线：BM25 + 向量相似度 + RRF 融合。

        Args:
            query: 检索查询文本
            limit: 返回结果数量上限
            session_key: 会话标识，用于记忆可见性过滤
            mem_type: 可选的记忆类型过滤
            episodes: 可选的 Episode 列表，参与统一排序
        Returns:
            按 RRF 分数降序排列的 (记忆条目|Episode, 分数) 列表
        """
        entries = self._entries_fn()
        if session_key and self._visibility_fn is not None:
            entries = [e for e in entries if self._visibility_fn(e, session_key)]
        if mem_type is not None:
            entries = [e for e in entries if e.type == mem_type]

        # Episodic candidates: use what the caller passed, else assemble them by
        # relevance via the injected search fn (semantic + LIKE fallback). This
        # keeps episodes on every retrieval path — prefetch warm-up included —
        # rather than only the inline sync-miss branch. mem_type filters to a
        # single memory type, so episodes don't belong in that query.
        if episodes is None and mem_type is None and self._episode_search_fn is not None:
            try:
                episodes = await self._episode_search_fn(
                    query, session_key, self._episode_candidate_limit
                )
            except Exception as e:
                logger.debug("Episodic candidate search failed: {}", e)
                episodes = None

        # Build unified candidate pool: memory entries + episodic proxies
        candidates: list = list(entries)
        ep_proxies: dict[str, Episode] = {}
        if episodes:
            for ep in episodes:
                proxy = _EpisodicProxy(
                    id=f"ep:{ep.id}", key="episode", content=ep.summary or "",
                    tags=[], _episode=ep,
                )
                candidates.append(proxy)
                ep_proxies[f"ep:{ep.id}"] = ep

        if not candidates:
            return []

        pool = limit * 3
        # BM25 ranking
        bm25_ranked = self._bm25_search(query, candidates, pool)
        bm25_rank_map = {eid: rank for rank, (eid, _) in enumerate(bm25_ranked)}

        # Vector ranking. The shared vector index also holds superseded /
        # cross-session / non-candidate episode vectors; filter those out
        # BEFORE enumerating so surviving candidates get contiguous ranks. If
        # we enumerated first and filtered after, a discarded hit would burn a
        # rank slot and understate the RRF score of the real top candidate.
        vec_rank_map: dict[str, int] = {}
        if self._vector_index and self._embed_fn:
            vec_results = await self._vector_search(query, pool)
            candidate_ids = {c.id for c in candidates}
            vec_rank_map = {
                eid: rank for rank, eid in enumerate(
                    eid for eid, _ in vec_results if eid in candidate_ids
                )
            }

        # RRF fusion
        all_ids = set(bm25_rank_map) | set(vec_rank_map)
        entry_map = {c.id: c for c in candidates}
        scored: list[tuple[MemoryEntry | Episode, float]] = []
        for cid in all_ids:
            candidate = entry_map.get(cid)
            if candidate is None or candidate.is_superseded:
                continue
            rrf = 0.0
            if cid in bm25_rank_map:
                rrf += 1.0 / (_RRF_K + bm25_rank_map[cid])
            if cid in vec_rank_map:
                rrf += 1.0 / (_RRF_K + vec_rank_map[cid])
            importance = self._forgetting.effective_importance(candidate)
            final_score = rrf * importance

            if final_score > 0:
                if isinstance(candidate, _EpisodicProxy) and candidate._episode:
                    scored.append((candidate._episode, final_score))
                else:
                    scored.append((candidate, final_score))

        scored.sort(key=lambda x: x[1], reverse=True)

        # Quota: memory <=5, episode <=3
        result: list[tuple[MemoryEntry | Episode, float]] = []
        mem_count = ep_count = 0
        for item, score in scored:
            if isinstance(item, Episode):
                if ep_count < 3:
                    result.append((item, score))
                    ep_count += 1
            else:
                if mem_count < 5:
                    result.append((item, score))
                    mem_count += 1
            if mem_count + ep_count >= limit:
                break
        logger.debug("RRF retrieve: {} candidates, {} results ({}mem+{}ep)",
                     len(scored), len(result), mem_count, ep_count)
        return result

    # -- tokenizer -----------------------------------------------------------

    @staticmethod
    def _tokenize(text: str) -> list[str]:
        return _tokenize_shared(text)

    # -- BM25 ----------------------------------------------------------------

    def _bm25_search(self, query: str, entries: list, limit: int) -> list[tuple[str, float]]:
        k1, b = 1.5, 0.75
        q_tokens = self._tokenize(query)
        if not q_tokens:
            return []
        doc_tokens = [self._tokenize(f"{e.key} {e.content} {' '.join(e.tags)}") for e in entries]
        n = len(entries)
        avgdl = sum(len(d) for d in doc_tokens) / max(n, 1)
        df: Counter[str] = Counter()
        for dt in doc_tokens:
            seen = set(dt)
            for qt in q_tokens:
                if qt in seen:
                    df[qt] += 1
        results: list[tuple[str, float]] = []
        for i, e in enumerate(entries):
            dl = len(doc_tokens[i])
            freq = Counter(doc_tokens[i])
            score = 0.0
            for qt in q_tokens:
                if qt not in freq:
                    continue
                idf = math.log((n - df[qt] + 0.5) / (df[qt] + 0.5) + 1.0)
                tf = (freq[qt] * (k1 + 1)) / (freq[qt] + k1 * (1 - b + b * dl / max(avgdl, 1e-9)))
                score += idf * tf
            if score > 0:
                results.append((e.id, score))
        results.sort(key=lambda x: x[1], reverse=True)
        return results[:limit]

    # -- vector search -------------------------------------------------------

    async def _vector_search(self, query: str, limit: int) -> list[tuple[str, float]]:
        if not self._embed_fn or not self._vector_index:
            return []
        try:
            embedding = await asyncio.wait_for(
                self._embed_fn(query), timeout=self._embed_timeout,
            )
        except (TimeoutError, asyncio.TimeoutError):
            # A silent quality downgrade is worse than the timeout itself —
            # tell the operator loudly once, then stay quiet.
            if not self._embed_timeout_warned:
                self._embed_timeout_warned = True
                logger.warning(
                    "Embedding call exceeded the {}s budget; memory retrieval degraded to "
                    "keyword-only for such turns. If your embedding endpoint is slow, raise "
                    "memory.embedTimeoutSeconds in your config.",
                    self._embed_timeout,
                )
            else:
                logger.debug("embed_fn exceeded {}s budget, degrading to keyword-only", self._embed_timeout)
            return []
        except Exception:
            logger.warning("embed_fn failed for vector search")
            return []
        return await self._vector_index.search(embedding, limit)

