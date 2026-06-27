"""Retrieval prefetch cache and freshness check.

The reply path reads a per-session cached retrieval result instead of running
the expensive hybrid retrieval inline. Freshness = recent enough (TTL) AND the
current query overlaps the cached query (Jaccard) — a topic shift is a miss.
"""
from __future__ import annotations

import asyncio
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
    # Task 13: episodic recall and knowledge retrieval folded into the same
    # per-session prefetch. Default None keeps every existing construction that
    # only fills `scored` backward-compatible.
    episodes: list[Any] | None = None
    knowledge_context: str | None = None


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
        episodic_fetch: Callable[[str, str], Awaitable[list]] | None = None,
        knowledge_fetch: Callable[[str, str], str] | None = None,
    ) -> None:
        self._retriever = retriever
        self._cache_put = cache_put
        self._limit = limit
        # episodic_fetch is async (SQL); knowledge_fetch is a SYNC CPU-bound
        # scan that must be offloaded to a thread so this background task never
        # blocks the event loop.
        self._episodic_fetch = episodic_fetch
        self._knowledge_fetch = knowledge_fetch

    async def prefetch(self, session_key: str, query: str, user_id: str = "") -> None:
        """Retrieve once for ``query`` and write the result into the cache.

        Main-memory, episodic and knowledge retrieval are warmed together into a
        single per-session entry. A background prefetch failure is swallowed and
        logged: the next turn simply misses the cache and falls back to inline
        retrieval, so a flaky retriever must not crash the post-reply task.

        Session isolation: episodic is scoped by ``session_key`` and knowledge by
        ``user_id`` — neither is allowed to cross sessions or users.
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
        episodes = await self._prefetch_episodes(session_key, query)
        knowledge_context = await self._prefetch_knowledge(user_id, query)
        entry = RetrievalCacheEntry(
            query_text=query,
            query_tokens=query_tokens(query),
            scored=list(scored or []),
            created_at=time.time(),
            episodes=episodes,
            knowledge_context=knowledge_context,
        )
        await self._cache_put(session_key, entry)

    async def _prefetch_episodes(self, session_key: str, query: str) -> list | None:
        """Warm episodic recall; isolated failure leaves episodes unset (None)."""
        if self._episodic_fetch is None:
            return None
        try:
            episodes = await self._episodic_fetch(query, session_key)
            return list(episodes) if episodes else None
        except Exception:
            logger.warning(
                "episodic prefetch failed for session %r", session_key, exc_info=True
            )
            return None

    async def _prefetch_knowledge(self, user_id: str, query: str) -> str | None:
        """Warm knowledge retrieval off the event loop (sync CPU-bound scan).

        Isolated failure leaves knowledge_context unset (None) so a knowledge
        error never costs us the episodic/main-memory prefetch.
        """
        if self._knowledge_fetch is None:
            return None
        try:
            loop = asyncio.get_running_loop()
            context = await loop.run_in_executor(
                None, lambda: self._knowledge_fetch(query, user_id)
            )
            return context or None
        except Exception:
            logger.warning(
                "knowledge prefetch failed for user %r", user_id, exc_info=True
            )
            return None
