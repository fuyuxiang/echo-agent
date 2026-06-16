"""Cost pricing: usage normalization + cost estimation."""

from __future__ import annotations

from echo_agent.cost.pricing import NormalizedUsage, normalize_usage, estimate_cost


def test_normalize_openai_fields():
    n = normalize_usage({"prompt_tokens": 100, "completion_tokens": 40}, "openai")
    assert n.input == 100
    assert n.output == 40
    assert n.total == 140


def test_normalize_anthropic_fields():
    n = normalize_usage(
        {"input_tokens": 80, "output_tokens": 20,
         "cache_read_input_tokens": 50, "cache_creation_input_tokens": 10},
        "anthropic",
    )
    assert n.input == 80
    assert n.output == 20
    assert n.cache_read == 50
    assert n.cache_write == 10


def test_normalize_openai_cached_dedup():
    # OpenAI prompt_tokens already INCLUDES cached tokens; cache_read must be
    # subtracted from input so it is not billed twice.
    n = normalize_usage(
        {"prompt_tokens": 100, "completion_tokens": 10,
         "prompt_tokens_details": {"cached_tokens": 30}},
        "openai",
    )
    assert n.cache_read == 30
    assert n.input == 70  # 100 - 30
    assert n.output == 10


def test_normalize_empty():
    n = normalize_usage({}, "openai")
    assert isinstance(n, NormalizedUsage)
    assert n.input == 0 and n.output == 0 and n.total == 0


def test_estimate_cost_known_model():
    # gpt-4o-mini snapshot: input 0.15 / output 0.60 per 1M.
    n = NormalizedUsage(input=1_000_000, output=1_000_000)
    cost = estimate_cost(n, "gpt-4o-mini", {})
    assert abs(cost - 0.75) < 1e-9


def test_estimate_cost_override_wins():
    n = NormalizedUsage(input=1_000_000, output=0)
    cost = estimate_cost(n, "my-model", {"my-model": {"input_per_1m": 2.0, "output_per_1m": 9.0}})
    assert abs(cost - 2.0) < 1e-9


def test_estimate_cost_unknown_model_zero():
    n = NormalizedUsage(input=1_000_000, output=1_000_000)
    cost = estimate_cost(n, "totally-unknown-model-xyz", {})
    assert cost == 0.0


def test_estimate_cost_dated_suffix_matches_longest_prefix():
    # OpenAI sends dated model ids like "gpt-4o-mini-2024-07-18".
    # Must resolve to gpt-4o-mini (0.15/0.60), NOT the shorter "gpt-4o" (2.50/10.00).
    n = NormalizedUsage(input=1_000_000, output=1_000_000)
    cost = estimate_cost(n, "gpt-4o-mini-2024-07-18", {})
    assert abs(cost - 0.75) < 1e-9  # 0.15 + 0.60, not 12.50


def test_estimate_cost_dated_gpt4o_still_matches():
    # A dated gpt-4o (not mini) should still resolve to gpt-4o.
    n = NormalizedUsage(input=1_000_000, output=0)
    cost = estimate_cost(n, "gpt-4o-2024-11-20", {})
    assert abs(cost - 2.50) < 1e-9
