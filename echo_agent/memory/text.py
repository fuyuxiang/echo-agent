"""Lightweight text tokenization helpers shared across memory retrieval paths."""

from __future__ import annotations

import re

# CJK runs are split into single chars + bigrams. Chinese has no whitespace
# word boundaries, so a latin-only tokenizer drops Chinese text to empty and
# keyword/BM25 retrieval returns nothing. Single chars guarantee recall;
# bigrams add cheap phrase locality. Zero-dependency approximation (no jieba).
_CJK_RE = re.compile(r"[一-鿿]+")

# Latin word tokens. CJK is handled separately by cjk_tokens because a
# latin-only regex drops Chinese text to empty.
_TOKEN_RE = re.compile(r"[a-z0-9]+")

# Authoritative stop-word table shared by every memory tokenization path
# (hybrid retrieval, query entropy, prefetch cache similarity). Keeping a
# single source prevents the table from drifting between callers — divergent
# tables make a cached query's token set and the real retrieval tokens
# disagree on what overlaps.
_STOP_WORDS = frozenset({
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "shall", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "into", "about", "it", "its",
    "this", "that", "and", "or", "but", "not", "no", "if", "so", "than",
})


def cjk_tokens(text: str) -> list[str]:
    """Split CJK runs in ``text`` into single chars plus adjacent bigrams."""
    tokens: list[str] = []
    for run in _CJK_RE.findall(text):
        tokens.extend(list(run))
        tokens.extend(run[i:i + 2] for i in range(len(run) - 1))
    return tokens


def tokenize(text: str) -> list[str]:
    """Lowercase token list: latin words (stop words removed) plus CJK tokens.

    Single source for keyword/BM25 tokenization. CJK tokens are not stop-word
    filtered because the stop-word table is latin-only, so filtering them is a
    no-op; they are appended verbatim from cjk_tokens.
    """
    lower = (text or "").lower()
    tokens = [t for t in _TOKEN_RE.findall(lower) if t not in _STOP_WORDS]
    tokens.extend(cjk_tokens(lower))
    return tokens


def is_discriminative(token: str) -> bool:
    """True when a token carries real relevance signal (not near-ubiquitous noise).

    Single CJK chars are the dominant false-match source in keyword retrieval:
    Chinese has no whitespace, so cjk_tokens emits every char, and a common char
    (的/是/我/了…) appears in a large fraction of the corpus — its IDF is low but
    nonzero, so in a small memory store (~1.5k entries) it still lets any entry
    sharing that one char enter the candidate pool. A CJK BIGRAM (len==2) or a
    multi-char latin token, by contrast, encodes a real phrase/word and is a
    trustworthy relevance signal. Single latin chars are likewise non-signal.

    This is the "at least one discriminative overlap" admission rule's predicate:
    a candidate matched ONLY by non-discriminative tokens is not admitted on
    lexical grounds alone (it may still be admitted via a vector-similarity hit).
    """
    return len(token) >= 2
