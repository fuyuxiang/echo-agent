"""Stateless model context-window resolution.

The context window (max input+output tokens a model accepts) is what the CLI
gauge shows as "max" and what the compressor uses to decide when to compress.
Providers do not report their own window on a response, so this module resolves
it from a layered set of sources, mirroring cost/pricing.py's prefix-match
registry pattern.

Resolution order (highest priority first):
  1. route/provider explicit override  (route_override, user knows best)
  2. setup-captured API metadata        (captured_windows, per-model)
  3. models.dev live catalog            (4000+ models, cached; blank on cold start)
  4. built-in registry                  (prefix match, ships with the app)
  5. global config default              (config_default) — only an explicit,
     non-zero user setting; a 0/unset default falls through so an unknown model
     lands on the honest 256K baseline instead of an arbitrary small number
  6. hard fallback DEFAULT_CONTEXT_WINDOW

The models.dev layer never blocks: it reads a background-refreshed cache and
returns 0 (fall through to the registry) until the cache is warm. Nothing here
mutates state; callers decide what to do with the number.
"""

from __future__ import annotations

# Hard fallback when a model matches nothing. 256K is a conservative modern
# baseline — big enough not to over-trigger compression on unknown models,
# small enough to stay honest about an unverified window.
DEFAULT_CONTEXT_WINDOW = 256_000

# Snapshot windows-2026-07 (input+output token budget). Substring match, so a
# provider-prefixed or dated id (e.g. "anthropic/claude-opus-4-20250101")
# still resolves to its family entry. Longest key wins to pick the most
# specific family. Extend as new models ship; user overrides always win.
_CONTEXT_WINDOWS: dict[str, int] = {
    # Anthropic
    "claude-opus-4": 200_000,
    "claude-sonnet-4": 200_000,
    "claude-haiku-4": 200_000,
    "claude-3-5-sonnet": 200_000,
    "claude-3-5-haiku": 200_000,
    "claude-3-opus": 200_000,
    "claude-3-sonnet": 200_000,
    "claude-3-haiku": 200_000,
    "claude-3": 200_000,
    "claude": 200_000,
    # OpenAI
    "gpt-4o-mini": 128_000,
    "gpt-4o": 128_000,
    "gpt-4.1": 1_047_576,
    "gpt-4-turbo": 128_000,
    "gpt-4": 8_192,
    "gpt-3.5-turbo": 16_385,
    "o3-mini": 200_000,
    "o3": 200_000,
    "o1-mini": 128_000,
    "o1": 200_000,
    # Google Gemini
    "gemini-1.5-pro": 2_000_000,
    "gemini-1.5-flash": 1_000_000,
    "gemini-2.5-pro": 1_000_000,
    "gemini-2.0-flash": 1_000_000,
    "gemini-2": 1_000_000,
    "gemini": 1_000_000,
    # DeepSeek / Qwen / Moonshot (common OpenAI-compatible endpoints)
    "deepseek": 64_000,
    "qwen": 128_000,
    "moonshot": 128_000,
    "kimi": 128_000,
    # MiniMax (OpenAI-compatible endpoint; setup capture misses its window, so
    # the registry is the cold-start pad until models.dev warms up). M3 is 1M;
    # the family catch-all stays conservative for older M-series models.
    "minimax-m3": 1_000_000,
    "minimax": 204_800,
}


def _match_registry(model: str) -> int:
    """Longest-key substring match against the built-in registry. 0 if none."""
    if not model:
        return 0
    lowered = model.lower()
    for key in sorted(_CONTEXT_WINDOWS, key=len, reverse=True):
        if key in lowered:
            return _CONTEXT_WINDOWS[key]
    return 0


def resolve_context_window(
    model: str,
    *,
    provider: str = "",
    route_override: int = 0,
    captured_windows: dict[str, int] | None = None,
    config_default: int = 0,
) -> int:
    """Resolve the real context window for ``model`` (see module docstring).

    All override/default args are optional and treated as "unset" when <= 0,
    so callers can pass whatever they have and let the layering decide.
    """
    # 1. Explicit route/provider override.
    if route_override and route_override > 0:
        return int(route_override)

    # 2. setup-captured API metadata (exact id, then substring).
    if captured_windows:
        win = captured_windows.get(model)
        if not win and model:
            lowered = model.lower()
            for cid, cwin in captured_windows.items():
                if cid and (cid.lower() in lowered or lowered in cid.lower()):
                    win = cwin
                    break
        if win and int(win) > 0:
            return int(win)

    # 3. models.dev live catalog (cached, non-blocking; 0 until the cache warms).
    #    Imported lazily so this stateless module stays import-light and the
    #    network/cache machinery only loads when actually resolving.
    try:
        from echo_agent.models.models_dev import lookup_context

        live = lookup_context(model, provider)
        if live and live > 0:
            return int(live)
    except Exception:  # noqa: BLE001 - catalog is best-effort, never fatal
        pass

    # 4. Built-in registry.
    registry = _match_registry(model)
    if registry > 0:
        return registry

    # 5. Global config default — only honor an explicit, non-zero user setting.
    #    A 0/unset default falls through to the honest 256K baseline so an
    #    unknown model is never pinned to an arbitrary small window.
    if config_default and config_default > 0:
        return int(config_default)

    # 6. Hard fallback.
    return DEFAULT_CONTEXT_WINDOW


def compression_window(display_window: int, cap: int) -> int:
    """The window the compressor should budget against.

    Displaying a model's true (possibly 1M+) window is honest, but budgeting
    compression against it would let context balloon before triggering — with
    a large cost/latency hit per request. ``cap`` (0 = uncapped) bounds the
    compression budget while the display stays truthful.
    """
    if cap and cap > 0:
        return min(display_window, cap)
    return display_window
