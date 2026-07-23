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
from echo_agent.memory.text import tokenize as _tokenize_shared, is_discriminative
from echo_agent.memory.eligibility import Audience, is_eligible

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
        is_unresolved_fn: Callable[[str], bool] | None = None,
        min_similarity: float = 0.25,
        rerank_fn: Callable[[str, list[str]], Awaitable[list[float] | None]] | None = None,
        rerank_top_k: int = 20,
        rerank_min_score: float | None = None,
    ):
        self._entries_fn = entries_fn
        # No detector wired ⇒ never treat anything as unresolved, so eligibility
        # filtering can't false-kill live entries when the caller omits it.
        self._is_unresolved_fn = is_unresolved_fn or (lambda _id: False)
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
        # Vector similarity floor. _vector_search returns (eid, score) but the
        # score was previously discarded (only rank was used), so any low-score
        # vector hit that entered the candidate pool still burned a rank slot and
        # contributed an RRF term — polluting the real candidates. Hits below
        # this floor are dropped before rank enumeration. BM25 side has no floor
        # (BM25 scores are a different scale). Tunable via memory.rrf_min_similarity.
        self._min_similarity = float(min_similarity)
        # Optional cross-encoder reranker. RRF only fuses rank ORDER; a
        # cross-encoder scores (query, doc) jointly — the precision gold standard
        # for "is this actually relevant". Applied to the fused top-K only (cheap)
        # AFTER RRF ordering and BEFORE the quota cut, so it reorders what gets
        # injected without touching candidate generation. None ⇒ off (pure RRF).
        # A failure/timeout inside rerank_fn returns None and keeps the RRF order:
        # reranking is a pure enhancement, never a recall gate.
        self._rerank_fn = rerank_fn
        self._rerank_top_k = max(1, int(rerank_top_k))
        # Optional absolute relevance floor on the cross-encoder score. Candidates
        # scoring below it are dropped after rerank (bounded — only within the
        # reranked top-K). None ⇒ reorder only, drop nothing.
        self._rerank_min_score = rerank_min_score

    async def retrieve(
        self, query: str, limit: int = 8,
        memory_scope: str = "", episode_session_key: str = "",
        mem_type: MemoryType | None = None,
        episodes: list[Episode] | None = None,
    ) -> list[tuple[MemoryEntry | Episode, float]]:
        """混合检索管线：BM25 + 向量相似度 + RRF 融合。

        Args:
            query: 检索查询文本
            limit: 返回结果数量上限
            memory_scope: 记忆可见性作用域，用于语义可见性过滤
            episode_session_key: 会话键，用于 episode 候选查询
            mem_type: 可选的记忆类型过滤
            episodes: 可选的 Episode 列表，参与统一排序
        Returns:
            按 RRF 分数降序排列的 (记忆条目|Episode, 分数) 列表
        """
        entries = self._entries_fn()
        if memory_scope and self._visibility_fn is not None:
            entries = [e for e in entries if self._visibility_fn(e, memory_scope)]
        if mem_type is not None:
            entries = [e for e in entries if e.type == mem_type]

        # Unified recall-eligibility gate, applied BEFORE BM25/vector ranking so
        # superseded/archived/unresolved entries never occupy a rank slot (which
        # would understate the RRF score of the real top candidate). This is the
        # authoritative filter; the fusion-loop check below is a second line.
        entries = [
            e for e in entries
            if is_eligible(e, Audience.RETRIEVAL, is_unresolved_fn=self._is_unresolved_fn)
        ]

        # Episodic candidates: use what the caller passed, else assemble them by
        # relevance via the injected search fn (semantic + LIKE fallback). This
        # keeps episodes on every retrieval path — prefetch warm-up included —
        # rather than only the inline sync-miss branch. mem_type filters to a
        # single memory type, so episodes don't belong in that query.
        if episodes is None and mem_type is None and self._episode_search_fn is not None:
            try:
                episodes = await self._episode_search_fn(
                    query, episode_session_key, self._episode_candidate_limit
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
        # BM25 ranking. strong_ids = candidates matched by a DISCRIMINATIVE query
        # token (CJK bigram / multi-char latin), i.e. lexically trustworthy hits.
        bm25_ranked, lexical_strong_ids = self._bm25_search(query, candidates, pool)
        bm25_rank_map = {eid: rank for rank, (eid, _) in enumerate(bm25_ranked)}

        # Vector ranking. The shared vector index also holds superseded /
        # cross-session / non-candidate episode vectors; filter those out
        # BEFORE enumerating so surviving candidates get contiguous ranks. If
        # we enumerated first and filtered after, a discarded hit would burn a
        # rank slot and understate the RRF score of the real top candidate.
        # Also drop hits below self._min_similarity: a low-similarity vector
        # match must not occupy a rank slot or contribute an RRF term.
        vec_rank_map: dict[str, int] = {}
        if self._vector_index and self._embed_fn:
            vec_results = await self._vector_search(query, pool)
            candidate_ids = {c.id for c in candidates}
            vec_rank_map = {
                eid: rank for rank, eid in enumerate(
                    eid for eid, score in vec_results
                    if eid in candidate_ids and score >= self._min_similarity
                )
            }

        # Relevance-admission gate (applied BEFORE RRF scoring). RRF is purely
        # rank-based — it discards the raw cosine magnitude, so the #1 candidate
        # gets full RRF weight whether it's a perfect match or the least-bad of a
        # garbage set. That is why recall "looks unrelated" when the store holds
        # no strongly-relevant entry: a lexical hit on a single common char, or a
        # low-cosine vector hit, still ranks #1 and gets injected. Fix: a
        # candidate is admitted only if it clears a real relevance bar on at
        # least one modality —
        #   • lexical:  matched a DISCRIMINATIVE query token (not a lone 的/是/我)
        #   • vector:   cosine ≥ self._min_similarity (already enforced in
        #               vec_rank_map construction above)
        # vec_rank_map only ever contains ids that passed the similarity floor,
        # so its keys ARE the vector-admitted set. A candidate matched only by a
        # single common char is in bm25_rank_map but neither strong nor vector-
        # admitted → dropped, instead of padding the quota.
        admitted = lexical_strong_ids | set(vec_rank_map)

        # RRF fusion
        all_ids = (set(bm25_rank_map) | set(vec_rank_map)) & admitted
        entry_map = {c.id: c for c in candidates}
        scored: list[tuple[MemoryEntry | Episode, float]] = []
        for cid in all_ids:
            candidate = entry_map.get(cid)
            if candidate is None or not is_eligible(
                candidate, Audience.RETRIEVAL, is_unresolved_fn=self._is_unresolved_fn
            ):
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

        # Cross-encoder rerank of the fused top-K (precision pass over the RRF
        # order). Cheap because it only sees the top-K, not the whole store. A
        # failure/timeout returns the list unchanged — pure enhancement.
        if self._rerank_fn is not None and scored:
            scored = await self._rerank(query, scored)

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

    # -- cross-encoder rerank ------------------------------------------------

    @staticmethod
    def _candidate_text(item: MemoryEntry | Episode) -> str:
        """Text handed to the cross-encoder: memory content, or episode summary."""
        if isinstance(item, Episode):
            return item.summary or ""
        key = getattr(item, "key", "")
        content = getattr(item, "content", "")
        return f"{key}: {content}" if key else content

    async def _rerank(
        self, query: str, scored: list[tuple[MemoryEntry | Episode, float]],
    ) -> list[tuple[MemoryEntry | Episode, float]]:
        """Reorder the fused top-K by cross-encoder relevance.

        Only the top-K (rerank_top_k) are scored; the tail keeps its RRF order and
        is appended after. On any failure the original list is returned unchanged
        (reranking never drops recall). When rerank_min_score is set, reranked
        candidates below it are dropped — but only within the scored head, so the
        floor can't silently empty a result that RRF considered relevant.
        """
        head = scored[: self._rerank_top_k]
        tail = scored[self._rerank_top_k :]
        docs = [self._candidate_text(item) for item, _ in head]
        try:
            rer_scores = await self._rerank_fn(query, docs)
        except Exception as e:
            logger.debug("Rerank failed, keeping RRF order: {}", e)
            return scored
        if not rer_scores or len(rer_scores) != len(head):
            # Misaligned/empty output is not trustworthy — keep RRF order.
            return scored
        reranked = list(zip(head, rer_scores))
        if self._rerank_min_score is not None:
            kept = [(pair, s) for pair, s in reranked if s >= self._rerank_min_score]
            # Guard: if the floor drops everything, fall back to the un-filtered
            # reranked head so a miscalibrated threshold can't zero out recall.
            if kept:
                reranked = kept
        reranked.sort(key=lambda x: x[1], reverse=True)
        # Carry the ORIGINAL RRF score forward as the item's score (rerank decides
        # order, not the surfaced score, which downstream treats as an RRF value).
        new_head = [pair for pair, _ in reranked]
        return new_head + tail

    # -- tokenizer -----------------------------------------------------------

    @staticmethod
    def _tokenize(text: str) -> list[str]:
        return _tokenize_shared(text)

    # -- BM25 ----------------------------------------------------------------

    def _bm25_search(
        self, query: str, entries: list, limit: int
    ) -> tuple[list[tuple[str, float]], set[str]]:
        """BM25 ranking.

        Returns (ranked, lexically_strong_ids). `ranked` is the usual top-`limit`
        (id, score) list used for RRF ordering. `lexically_strong_ids` is the
        subset of candidates that matched at least one DISCRIMINATIVE query token
        (a CJK bigram or a multi-char latin word — see text.is_discriminative).
        The admission gate uses it so a candidate matched ONLY by a single common
        char (的/是/我…) is NOT admitted on lexical grounds — the root cause of
        "recall looks unrelated". When the query itself has no discriminative
        token (e.g. a bare single-char query), the gate is a no-op: every matched
        candidate is treated as strong so we never zero out such a query.
        """
        k1, b = 1.5, 0.75
        q_tokens = self._tokenize(query)
        if not q_tokens:
            return [], set()
        disc_q = {qt for qt in q_tokens if is_discriminative(qt)}
        gate_active = bool(disc_q)
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
        strong_ids: set[str] = set()
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
                if not gate_active or disc_q & freq.keys():
                    strong_ids.add(e.id)
        results.sort(key=lambda x: x[1], reverse=True)
        return results[:limit], strong_ids

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

