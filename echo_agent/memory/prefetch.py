"""Retrieval prefetch cache and freshness check.

The reply path reads a per-session cached retrieval result instead of running
the expensive hybrid retrieval inline. Freshness = recent enough (TTL) AND the
current query overlaps the cached query (Jaccard) — a topic shift is a miss.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from echo_agent.memory.text import tokenize


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
