"""Retrieval prefetch cache and freshness check.

The reply path reads a per-session cached retrieval result instead of running
the expensive hybrid retrieval inline. Freshness = recent enough (TTL) AND the
current query overlaps the cached query (Jaccard) — a topic shift is a miss.
"""
from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from echo_agent.memory.text import cjk_tokens

# Mirror HybridRetriever._tokenize (retrieval.py:117): latin word tokens plus
# CJK chars/bigrams. cjk_tokens alone drops English text to empty, so a
# query-token set built on it would never overlap for latin queries.
_TOKEN_RE = re.compile(r"[a-z0-9]+")

_STOP = frozenset({
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "shall", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "into", "about", "it", "its",
    "this", "that", "and", "or", "but", "not", "no", "if", "so", "than",
    "how", "what", "my",
})


def query_tokens(text: str) -> frozenset[str]:
    """Lowercase token set (latin words + CJK chars/bigrams), stop words removed."""
    lower = (text or "").lower()
    toks = {t for t in _TOKEN_RE.findall(lower) if t not in _STOP}
    toks.update(t for t in cjk_tokens(lower) if t and t not in _STOP)
    return frozenset(toks)


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
