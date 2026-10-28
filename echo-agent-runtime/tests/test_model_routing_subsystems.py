"""M3-A model routing — non-inference subsystems route through ModelRouter.

Pins the wiring that routes compression / approval / evolution / embedding by
task_type through ``ModelRouter`` instead of hardcoding the main provider, and
the embedding fallback that fixes the silent-None gap when the main provider
(e.g. Anthropic) has no ``embed`` implementation.
"""

from __future__ import annotations

import pytest

from echo_agent.config.schema import ModelRouteConfig, ModelsConfig, ProviderConfig
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.models.router import ModelRouter


class _NoEmbedProvider(LLMProvider):
    """A main provider with no embedding support (Anthropic-like)."""

    def __init__(self, name: str = "main"):
        super().__init__()
        self.name = name

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok")

    def get_default_model(self) -> str:
        return "main-model"


class _EmbedProvider(LLMProvider):
    """An OpenAI-like provider that supports embeddings."""

    def __init__(self, name: str = "openai"):
        super().__init__()
        self.name = name
        self.embed_calls: list[tuple[str, str | None]] = []

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok")

    def get_default_model(self) -> str:
        return "gpt-x"

    async def embed(self, text: str, model: str | None = None):
        self.embed_calls.append((text, model))
        return [0.1, 0.2, 0.3]


def _models_config(routes=None, providers=None) -> ModelsConfig:
    return ModelsConfig(
        default_model="main-model",
        providers=providers or [],
        routes=routes or [],
    )


# ── resolve() ─────────────────────────────────────────────────────────────


def test_resolve_falls_back_when_no_route_matches():
    router = ModelRouter(_models_config())
    main = _NoEmbedProvider()
    router.register_provider("main", main)
    provider, model = router.resolve(
        "compression", fallback_provider=main, fallback_model="main-model"
    )
    assert provider is main
    assert model == "main-model"


def test_resolve_uses_matching_route():
    routes = [ModelRouteConfig(model="cheap-model", provider="aux", task_types=["compression"])]
    providers = [ProviderConfig(name="aux", models=["cheap-model"])]
    router = ModelRouter(_models_config(routes=routes, providers=providers))
    main = _NoEmbedProvider()
    aux = _NoEmbedProvider("aux")
    router.register_provider("main", main)
    router.register_provider("aux", aux)
    provider, model = router.resolve(
        "compression", fallback_provider=main, fallback_model="main-model"
    )
    assert provider is aux
    assert model == "cheap-model"


def test_resolve_unmatched_task_keeps_fallback():
    routes = [ModelRouteConfig(model="cheap-model", provider="aux", task_types=["compression"])]
    providers = [ProviderConfig(name="aux", models=["cheap-model"])]
    router = ModelRouter(_models_config(routes=routes, providers=providers))
    main = _NoEmbedProvider()
    router.register_provider("main", main)
    router.register_provider("aux", _NoEmbedProvider("aux"))
    # approval has no route → fallback
    provider, model = router.resolve(
        "approval", fallback_provider=main, fallback_model="smart-model"
    )
    assert provider is main
    assert model == "smart-model"


# ── find_embed_provider() ───────────────────────────────────────────────────


def test_find_embed_provider_picks_embed_capable():
    router = ModelRouter(_models_config())
    router.register_provider("main", _NoEmbedProvider())
    emb = _EmbedProvider()
    router.register_provider("openai", emb)
    provider, model = router.find_embed_provider("text-embed-3")
    assert provider is emb
    assert model == "text-embed-3"


def test_find_embed_provider_none_when_no_capable_provider():
    router = ModelRouter(_models_config())
    router.register_provider("main", _NoEmbedProvider())
    provider, _ = router.find_embed_provider("text-embed-3")
    assert provider is None


def test_find_embed_provider_honours_explicit_route():
    routes = [ModelRouteConfig(model="embed-big", provider="openai", task_types=["embedding"])]
    router = ModelRouter(_models_config(routes=routes))
    router.register_provider("main", _NoEmbedProvider())
    emb = _EmbedProvider()
    router.register_provider("openai", emb)
    provider, model = router.find_embed_provider("default-embed")
    assert provider is emb
    assert model == "embed-big"


# ── summarizer routing ───────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_summarizer_routes_compression_to_aux_provider():
    from echo_agent.agent.compression.summarizer import LLMSummarizer
    from echo_agent.agent.compression.types import CompressionStats

    routes = [ModelRouteConfig(model="cheap-model", provider="aux", task_types=["compression"])]
    providers = [ProviderConfig(name="aux", models=["cheap-model"])]
    router = ModelRouter(_models_config(routes=routes, providers=providers))
    main = _NoEmbedProvider()

    class _RecordingProvider(_NoEmbedProvider):
        def __init__(self):
            super().__init__("aux")
            self.models_seen: list[str | None] = []

        async def chat_with_retry(self, messages, model=None, **kwargs):
            self.models_seen.append(model)
            return LLMResponse(content="summary text")

    aux = _RecordingProvider()
    router.register_provider("main", main)
    router.register_provider("aux", aux)

    summarizer = LLMSummarizer(
        provider=main,
        summary_model="",
        default_model="main-model",
        summary_target_ratio=0.3,
        summary_min_tokens=10,
        summary_max_tokens=500,
        cooldown_seconds=0,
        router=router,
    )
    out = await summarizer.summarize(
        [{"role": "user", "content": "hello there, summarize me please"}],
        focus_topic="",
        stats=CompressionStats(),
        token_estimator=lambda m: 100,
    )
    assert out == "summary text"
    assert aux.models_seen == ["cheap-model"]
