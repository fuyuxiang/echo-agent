"""Retrieval prefetch cache and freshness check.

The reply path reads a per-session cached retrieval result instead of running
the expensive hybrid retrieval inline. Freshness = recent enough (TTL) AND the
current query overlaps the cached query (Jaccard) — a topic shift is a miss.
"""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from typing import Any, Awaitable, Callable

from echo_agent.memory.text import tokenize

logger = logging.getLogger(__name__)


def query_tokens(text: str) -> frozenset[str]:
    """Lowercase token set (latin words + CJK chars/bigrams), stop words removed.

    Shares echo_agent.memory.text.tokenize with hybrid retrieval so the cache's
    query-similarity token set and the real retrieval tokens use one stop-word
    table and one tokenization. Set semantics (dedup) is the only difference
    from the list returned by tokenize.
    """
    return frozenset(tokenize(text))


@dataclass
class RetrievalCacheEntry:
    query_text: str
    query_tokens: frozenset[str]
    scored: list[Any]
    created_at: float


def is_fresh(entry: RetrievalCacheEntry, query: str, *, now: float,
             ttl: float, jaccard_min: float) -> bool:
    """True when the cached entry is recent (TTL) and overlaps the query (Jaccard)."""
    if now - entry.created_at >= ttl:
        return False
    cur = query_tokens(query)
    if not cur or not entry.query_tokens:
        return False
    inter = len(cur & entry.query_tokens)
    union = len(cur | entry.query_tokens)
    jaccard = inter / union if union else 0.0
    return jaccard >= jaccard_min


class RetrievalPrefetcher:
    """Run hybrid retrieval off the reply path and store it as a cache entry.

    Wired (Task 12) to fire after a reply so the next turn can read a fresh
    cached result instead of running retrieval inline.
    """

    def __init__(
        self,
        retriever: Any,
        cache_put: Callable[[str, "RetrievalCacheEntry"], Awaitable[None]],
        *,
        limit: int = 5,
    ) -> None:
        self._retriever = retriever
        self._cache_put = cache_put
        self._limit = limit

    async def prefetch(self, session_key: str, query: str) -> None:
        """Retrieve once for ``query`` and write the result into the cache.

        A background prefetch failure is swallowed and logged: the next turn
        simply misses the cache and falls back to inline retrieval, so a flaky
        retriever must not crash the post-reply task.
        """
        try:
            scored = await self._retriever.retrieve(
                query, limit=self._limit, session_key=session_key
            )
        except Exception:
            logger.warning(
                "retrieval prefetch failed for session %r", session_key, exc_info=True
            )
            return
        entry = RetrievalCacheEntry(
            query_text=query,
            query_tokens=query_tokens(query),
            scored=list(scored or []),
            created_at=time.time(),
        )
        await self._cache_put(session_key, entry)
