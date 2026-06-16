"""Stateless cost pricing: usage normalization + per-model cost estimation."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class NormalizedUsage:
    """Provider-agnostic token usage. input excludes cache_read (already deducted)."""
    input: int = 0
    output: int = 0
    cache_read: int = 0
    cache_write: int = 0

    @property
    def total(self) -> int:
        return self.input + self.output + self.cache_read + self.cache_write


def normalize_usage(usage: dict, provider: str = "") -> NormalizedUsage:
    """Unify OpenAI (prompt/completion) and Anthropic (input/output) usage dicts.

    OpenAI prompt_tokens already includes cached tokens, so cache_read is
    subtracted from input to avoid double counting. Anthropic reports cache
    separately and input_tokens does not include it.
    """
    if not usage:
        return NormalizedUsage()

    cache_read = int(
        usage.get("cache_read_input_tokens")
        or (usage.get("prompt_tokens_details") or {}).get("cached_tokens")
        or 0
    )
    cache_write = int(usage.get("cache_creation_input_tokens") or 0)

    if "prompt_tokens" in usage or "completion_tokens" in usage:
        # OpenAI style: prompt_tokens includes cached -> subtract.
        raw_input = int(usage.get("prompt_tokens") or 0)
        inp = max(0, raw_input - cache_read)
        out = int(usage.get("completion_tokens") or 0)
    else:
        # Anthropic style: input_tokens excludes cache.
        inp = int(usage.get("input_tokens") or 0)
        out = int(usage.get("output_tokens") or 0)

    return NormalizedUsage(input=inp, output=out, cache_read=cache_read, cache_write=cache_write)
