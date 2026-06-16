"""Lightweight text tokenization helpers shared across memory retrieval paths."""

from __future__ import annotations

import re

# CJK runs are split into single chars + bigrams. Chinese has no whitespace
# word boundaries, so a latin-only tokenizer drops Chinese text to empty and
# keyword/BM25 retrieval returns nothing. Single chars guarantee recall;
# bigrams add cheap phrase locality. Zero-dependency approximation (no jieba).
_CJK_RE = re.compile(r"[一-鿿]+")


def cjk_tokens(text: str) -> list[str]:
    """Split CJK runs in ``text`` into single chars plus adjacent bigrams."""
    tokens: list[str] = []
    for run in _CJK_RE.findall(text):
        tokens.extend(list(run))
        tokens.extend(run[i:i + 2] for i in range(len(run) - 1))
    return tokens
