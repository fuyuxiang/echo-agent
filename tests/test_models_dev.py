"""Tests for echo_agent.models.models_dev — the live context-window catalog.

All tests run offline: the network fetch is monkeypatched and lookups use
``blocking=True`` so a synchronous refresh runs in-thread (no daemon threads,
deterministic). The on-disk cache is redirected to a tmp dir.
"""

from __future__ import annotations

import json

import pytest

from echo_agent.models import models_dev


_SAMPLE_CATALOG = {
    "minimax": {
        "name": "MiniMax",
        "models": {
            "MiniMax-M3": {"limit": {"context": 1_000_000, "output": 128_000}},
            "abab6.5": {"limit": {"context": 245_760}},
        },
    },
    "openai": {
        "name": "OpenAI",
        "models": {
            "gpt-4o": {"limit": {"context": 128_000}},
            "tts-1": {"limit": {"context": 0}},  # audio model, no real window
        },
    },
    "google": {
        "name": "Google",
        "models": {
            "gemini-2.5-pro": {"limit": {"context": 1_048_576}},
        },
    },
}


@pytest.fixture(autouse=True)
def _isolate(tmp_path, monkeypatch):
    """Fresh module state + a tmp cache dir for every test."""
    monkeypatch.setattr(models_dev, "echo_home", lambda: tmp_path)
    models_dev._reset_for_tests()
    yield
    models_dev._reset_for_tests()


def _patch_fetch(monkeypatch, payload):
    calls = {"n": 0}

    def _fake(*_a, **_k):
        calls["n"] += 1
        return payload

    monkeypatch.setattr(models_dev, "_fetch_from_network", _fake)
    return calls


class TestLookup:
    def test_resolves_via_mapped_provider(self, monkeypatch):
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("MiniMax-M3", "minimax", blocking=True) == 1_000_000

    def test_provider_alias_mapping(self, monkeypatch):
        # "qwen"/"gemini" map to models.dev ids "alibaba"/"google".
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("gemini-2.5-pro", "gemini", blocking=True) == 1_048_576

    def test_case_insensitive_model_match(self, monkeypatch):
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("minimax-m3", "minimax", blocking=True) == 1_000_000

    def test_unknown_provider_scans_all(self, monkeypatch):
        # No/unknown provider still finds the model by scanning every provider.
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("gpt-4o", "", blocking=True) == 128_000
        assert models_dev.lookup_context("gpt-4o", "some-unknown", blocking=True) == 128_000

    def test_zero_context_is_treated_as_miss(self, monkeypatch):
        # Audio/image models with limit.context == 0 must not resolve.
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("tts-1", "openai", blocking=True) == 0

    def test_missing_model_returns_zero(self, monkeypatch):
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("nonexistent-model", "openai", blocking=True) == 0

    def test_empty_model_returns_zero(self, monkeypatch):
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        assert models_dev.lookup_context("", "openai", blocking=True) == 0


class TestCacheTiers:
    def test_network_result_is_persisted_to_disk(self, monkeypatch):
        _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        models_dev.lookup_context("gpt-4o", "openai", blocking=True)
        cache_file = models_dev._cache_path()
        assert cache_file.exists()
        on_disk = json.loads(cache_file.read_text(encoding="utf-8"))
        assert on_disk["openai"]["models"]["gpt-4o"]["limit"]["context"] == 128_000

    def test_memory_cache_avoids_refetch(self, monkeypatch):
        calls = _patch_fetch(monkeypatch, _SAMPLE_CATALOG)
        models_dev.lookup_context("gpt-4o", "openai", blocking=True)
        models_dev.lookup_context("gpt-4o", "openai", blocking=True)
        # Second lookup is served from the warm in-memory cache.
        assert calls["n"] == 1

    def test_disk_cache_loaded_on_cold_start(self, monkeypatch):
        # Seed disk, clear memory: a fresh lookup must read disk, not network.
        models_dev._save_disk_cache(_SAMPLE_CATALOG)
        models_dev._reset_for_tests()
        calls = _patch_fetch(monkeypatch, {})  # network would return nothing
        assert models_dev.lookup_context("gpt-4o", "openai", blocking=True) == 128_000
        # Disk was fresh (just written), so no network fetch was needed.
        assert calls["n"] == 0

    def test_network_failure_falls_back_to_stale_disk(self, monkeypatch):
        # Stale disk cache + failing network → still resolves from disk.
        models_dev._save_disk_cache(_SAMPLE_CATALOG)
        models_dev._reset_for_tests()
        # Force the disk entry to look stale so a refresh is attempted.
        monkeypatch.setattr(models_dev, "_CACHE_TTL", -1)
        _patch_fetch(monkeypatch, {})  # network down
        assert models_dev.lookup_context("gpt-4o", "openai", blocking=True) == 128_000

    def test_cold_offline_returns_zero(self, monkeypatch):
        # No disk cache, network down: a miss returns 0 (registry covers it).
        _patch_fetch(monkeypatch, {})
        assert models_dev.lookup_context("gpt-4o", "openai", blocking=True) == 0


class TestExtractContext:
    def test_valid(self):
        assert models_dev._extract_context({"limit": {"context": 200_000}}) == 200_000

    def test_missing_limit(self):
        assert models_dev._extract_context({}) == 0

    def test_non_dict_limit(self):
        assert models_dev._extract_context({"limit": "nope"}) == 0

    def test_zero_context(self):
        assert models_dev._extract_context({"limit": {"context": 0}}) == 0
