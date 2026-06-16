"""Cost pricing: usage normalization + cost estimation."""

from __future__ import annotations

from echo_agent.cost.pricing import NormalizedUsage, normalize_usage


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
