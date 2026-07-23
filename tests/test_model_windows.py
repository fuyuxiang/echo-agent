"""Tests for echo_agent.models.model_windows — layered window resolution."""

from __future__ import annotations

import pytest

from echo_agent.models.model_windows import (
    DEFAULT_CONTEXT_WINDOW,
    compression_window,
    resolve_context_window,
)


@pytest.fixture(autouse=True)
def _no_models_dev(monkeypatch):
    """Neutralize the live models.dev layer by default.

    The resolver consults models.dev between the captured and registry layers.
    In unit tests we force it to miss (return 0) so the other layers are tested
    in isolation and no background network thread is spawned. Tests that target
    the models.dev layer override this patch explicitly.
    """
    monkeypatch.setattr(
        "echo_agent.models.models_dev.lookup_context",
        lambda model, provider="", **kw: 0,
    )


class TestResolvePriority:
    def test_route_override_wins_over_everything(self):
        win = resolve_context_window(
            "claude-opus-4",
            route_override=12345,
            captured_windows={"claude-opus-4": 999999},
            config_default=65536,
        )
        assert win == 12345

    def test_captured_metadata_beats_registry_and_default(self):
        # A captured window for the exact id beats the built-in registry value.
        win = resolve_context_window(
            "gpt-4o",
            captured_windows={"gpt-4o": 111111},
            config_default=65536,
        )
        assert win == 111111

    def test_captured_substring_match(self):
        # Captured ids match by substring when the exact id is absent.
        win = resolve_context_window(
            "gpt-4o-2024-11-20",
            captured_windows={"gpt-4o": 128000},
        )
        assert win == 128000

    def test_registry_prefix_match(self):
        # Dated/provider-prefixed ids still resolve via the registry.
        assert resolve_context_window("anthropic/claude-opus-4-20250101") == 200_000
        assert resolve_context_window("gpt-4o-mini-2024-07-18") == 128_000

    def test_registry_longest_key_wins(self):
        # "gpt-4o-mini" (longer key) must win over "gpt-4o" for a mini id.
        assert resolve_context_window("gpt-4o-mini") == 128_000

    def test_minimax_m3_registry_pad(self):
        # MiniMax-M3 must resolve to its real 1M window from the registry pad,
        # not fall through to the config default (regression for the 65.5K bug).
        assert resolve_context_window("MiniMax-M3") == 1_000_000
        # The family catch-all covers older M-series ids.
        assert resolve_context_window("minimax-abab6") == 204_800

    def test_explicit_config_default_when_no_registry_match(self):
        # A positive config_default is still honored for an unknown model.
        win = resolve_context_window("some-unknown-model-xyz", config_default=54321)
        assert win == 54321

    def test_unknown_model_lands_on_baseline_not_small_default(self):
        # Regression: with config_default unset (0), an unknown model must land
        # on the honest 256K baseline, never an arbitrary small window.
        win = resolve_context_window("some-unknown-model-xyz", config_default=0)
        assert win == DEFAULT_CONTEXT_WINDOW

    def test_hard_fallback_when_nothing_matches(self):
        win = resolve_context_window("some-unknown-model-xyz")
        assert win == DEFAULT_CONTEXT_WINDOW

    def test_zero_and_empty_overrides_are_ignored(self):
        # 0/empty means "unset" and must fall through, not short-circuit.
        win = resolve_context_window(
            "gpt-4o",
            route_override=0,
            captured_windows={},
            config_default=0,
        )
        assert win == 128_000  # registry


class TestModelsDevLayer:
    def test_models_dev_beats_registry(self, monkeypatch):
        # A live models.dev hit takes priority over the built-in registry.
        monkeypatch.setattr(
            "echo_agent.models.models_dev.lookup_context",
            lambda model, provider="", **kw: 1_048_576,
        )
        # gpt-4o is 128K in the registry; models.dev override must win.
        assert resolve_context_window("gpt-4o") == 1_048_576

    def test_models_dev_below_captured_override(self, monkeypatch):
        # Captured metadata (setup) still outranks models.dev.
        monkeypatch.setattr(
            "echo_agent.models.models_dev.lookup_context",
            lambda model, provider="", **kw: 999,
        )
        win = resolve_context_window("gpt-4o", captured_windows={"gpt-4o": 111111})
        assert win == 111111

    def test_models_dev_miss_falls_through_to_registry(self, monkeypatch):
        monkeypatch.setattr(
            "echo_agent.models.models_dev.lookup_context",
            lambda model, provider="", **kw: 0,
        )
        assert resolve_context_window("gpt-4o") == 128_000

    def test_models_dev_error_is_swallowed(self, monkeypatch):
        # A catalog failure must never break resolution — fall through cleanly.
        def _boom(model, provider="", **kw):
            raise RuntimeError("catalog down")

        monkeypatch.setattr(
            "echo_agent.models.models_dev.lookup_context", _boom
        )
        assert resolve_context_window("gpt-4o") == 128_000


class TestCompressionWindow:
    def test_cap_bounds_the_window(self):
        assert compression_window(1_000_000, 200_000) == 200_000

    def test_below_cap_unchanged(self):
        assert compression_window(128_000, 200_000) == 128_000

    def test_zero_cap_means_uncapped(self):
        assert compression_window(1_000_000, 0) == 1_000_000
