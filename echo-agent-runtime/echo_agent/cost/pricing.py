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


@dataclass
class ModelPrice:
    """USD per 1M tokens."""
    input_per_1m: float
    output_per_1m: float
    cache_read_per_1m: float = 0.0
    cache_write_per_1m: float = 0.0


# Snapshot pricing-2026-06 (USD per 1M tokens). Override via config cost.pricingOverrides.
_PRICING_SNAPSHOT: dict[str, ModelPrice] = {
    "gpt-4o": ModelPrice(2.50, 10.00, 1.25),
    "gpt-4o-mini": ModelPrice(0.15, 0.60, 0.075),
    "o3-mini": ModelPrice(1.10, 4.40),
    "claude-3-5-sonnet": ModelPrice(3.00, 15.00, 0.30, 3.75),
    "claude-3-5-haiku": ModelPrice(0.80, 4.00, 0.08, 1.00),
    "claude-3-opus": ModelPrice(15.00, 75.00, 1.50, 18.75),
    "gemini-1.5-pro": ModelPrice(1.25, 5.00),
    "gemini-1.5-flash": ModelPrice(0.075, 0.30),
}

_missing_price_warned: set[str] = set()


def _resolve_price(model: str, overrides: dict) -> ModelPrice | None:
    ov = (overrides or {}).get(model)
    if ov:
        return ModelPrice(
            input_per_1m=float(ov.get("input_per_1m", 0.0)),
            output_per_1m=float(ov.get("output_per_1m", 0.0)),
            cache_read_per_1m=float(ov.get("cache_read_per_1m", 0.0)),
            cache_write_per_1m=float(ov.get("cache_write_per_1m", 0.0)),
        )
    if model in _PRICING_SNAPSHOT:
        return _PRICING_SNAPSHOT[model]
    # Longest-prefix match so "gpt-4o-mini-2024-..." resolves to "gpt-4o-mini",
    # not the shorter "gpt-4o". OpenAI/Anthropic ship dated model ids.
    for key in sorted(_PRICING_SNAPSHOT, key=len, reverse=True):
        if model.startswith(key):
            return _PRICING_SNAPSHOT[key]
    return None


def estimate_cost(usage: NormalizedUsage, model: str, overrides: dict | None = None) -> float:
    """Estimate USD cost. Unknown model -> 0.0 + one-time warning (does not block metering)."""
    price = _resolve_price(model, overrides or {})
    if price is None:
        if model not in _missing_price_warned:
            from loguru import logger
            logger.warning("No pricing for model {!r}; cost counted as 0. Set cost.pricingOverrides.", model)
            _missing_price_warned.add(model)
        return 0.0
    return (
        usage.input * price.input_per_1m
        + usage.output * price.output_per_1m
        + usage.cache_read * price.cache_read_per_1m
        + usage.cache_write * price.cache_write_per_1m
    ) / 1_000_000
