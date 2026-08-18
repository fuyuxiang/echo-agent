"""Tests for the provider brand catalog."""
from __future__ import annotations

from echo_agent.cli.setup import providers as p

_VALID_DIALECTS = {"openai", "anthropic", "gemini", "bedrock", "openrouter"}


def test_catalog_nonempty_and_unique_ids():
    ids = [e.id for e in p.CATALOG]
    assert ids
    assert len(ids) == len(set(ids))


def test_every_entry_has_valid_dialect():
    for e in p.CATALOG:
        assert e.dialect in _VALID_DIALECTS, e.id


def test_openai_dialect_compat_entries_have_api_base():
    # domestic + local + aggregator OpenAI-compat entries must ship a base URL
    # unless they explicitly require the user to type one.
    for e in p.CATALOG:
        if e.dialect == "openai" and e.id != "openai":
            assert e.api_base or e.needs_api_base, e.id


def test_expected_domestic_providers_present():
    ids = {e.id for e in p.CATALOG}
    assert {"deepseek", "qwen", "kimi", "glm"} <= ids


def test_expected_local_and_aggregator_present():
    ids = {e.id for e in p.CATALOG}
    assert {
        "ollama", "lmstudio", "openrouter", "siliconflow", "atlascloud",
        "custom", "bedrock",
    } <= ids


def test_atlascloud_uses_openai_compatible_endpoint():
    entry = p.find("atlascloud")

    assert entry.dialect == "openai"
    assert entry.api_base == "https://api.atlascloud.ai/v1"
    assert entry.models_endpoint == "https://api.atlascloud.ai/v1/models"
    assert entry.api_key_env_vars == ("ATLASCLOUD_API_KEY",)
    assert entry.fallback_models == ["qwen/qwen3.8-max"]


def test_grouped_preserves_all_entries_and_order():
    groups = p.grouped_catalog()
    flat = [e.id for _label, entries in groups for e in entries]
    assert set(flat) == {e.id for e in p.CATALOG}
    # first group is mainstream, containing openai first
    assert groups[0][1][0].id == "openai"


def test_find_returns_entry_or_none():
    assert p.find("deepseek").dialect == "openai"
    assert p.find("nope") is None


def test_fallback_models_present_for_mainstream():
    assert p.find("openai").fallback_models
    assert p.find("anthropic").fallback_models
