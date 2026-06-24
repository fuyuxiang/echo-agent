"""Characterization tests for ModelRouter failover selection.

补测覆盖缺口（优先级5，models/router.py）：
- mark_failure 连续 ≥3 → cooldown，且不再被 _find_healthy_provider_entry 选中
- _find_healthy_provider_entry 四级回退链（preferred / config 支持模型 / 默认模型匹配 / 无模型兜底）
- resolve 非推理子系统路由（task_type="approval" / "compression"）
- _provider_available cooldown 窗口过后恢复可用

性质：表征测试，不改源码；以实际行为为准。
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

from echo_agent.config.schema import ModelRouteConfig, ModelsConfig, ProviderConfig
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.models.router import ModelRouter, HealthStatus


class _FakeProvider(LLMProvider):
    def __init__(self, model: str = "fake-model"):
        super().__init__()
        self._model = model

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok", finish_reason="stop")

    def get_default_model(self) -> str:
        return self._model


# ── mark_failure 状态机：≥3 → cooldown → 不再被选中 ────────────────────────────


def test_mark_failure_three_times_enters_cooldown_and_not_selected() -> None:
    config = ModelsConfig(
        default_model="m1",
        providers=[ProviderConfig(name="primary", models=["m1"])],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    router.register_provider("primary", _FakeProvider("m1"))

    # 前两次只降级
    router.mark_failure("primary", "e1")
    router.mark_failure("primary", "e2")
    assert router._health["primary"].status == HealthStatus.DEGRADED
    # degraded 仍可被选中
    assert router._find_healthy_provider_entry("m1") is not None

    # 第三次进入 cooldown
    router.mark_failure("primary", "e3")
    assert router._health["primary"].status == HealthStatus.COOLDOWN
    # cooldown 且窗口未过 → 不再被 _find_healthy_provider_entry 选中
    assert router._find_healthy_provider_entry("m1") is None


# ── _find_healthy_provider_entry 四级回退链 ───────────────────────────────────


def _multi_provider_router() -> ModelRouter:
    config = ModelsConfig(
        default_model="m1",
        providers=[
            ProviderConfig(name="alpha", models=["alpha-model"]),
            ProviderConfig(name="beta", models=["beta-model"]),
        ],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    router.register_provider("alpha", _FakeProvider("alpha-model"))
    router.register_provider("beta", _FakeProvider("beta-model"))
    return router


def test_find_entry_level1_preferred_hit() -> None:
    """第一级：preferred_provider 已注册且可用 → 直接命中。"""
    router = _multi_provider_router()
    entry = router._find_healthy_provider_entry("beta-model", preferred_provider="alpha")
    assert entry is not None
    # 即便 model 是 beta-model，preferred=alpha 健康即优先返回 alpha
    assert entry[0] == "alpha"


def test_find_entry_level2_config_supports_model() -> None:
    """第二级：无 preferred，按 config.providers 的 models 列表匹配。"""
    router = _multi_provider_router()
    entry = router._find_healthy_provider_entry("beta-model")
    assert entry is not None
    assert entry[0] == "beta"


def test_find_entry_level3_default_model_match() -> None:
    """第三级：config 不声明该模型，但某 provider 的 get_default_model 匹配。"""
    config = ModelsConfig(
        default_model="solo-model",
        # provider 不在 config.providers 列表中声明 models，使第二级落空
        providers=[],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    router.register_provider("solo", _FakeProvider("solo-model"))
    entry = router._find_healthy_provider_entry("solo-model")
    assert entry is not None
    assert entry[0] == "solo"


def test_find_entry_level4_no_model_fallback() -> None:
    """第四级：model 为空 → 返回首个可用 provider 兜底。"""
    config = ModelsConfig(default_model="", providers=[], routes=[])
    router = ModelRouter(config, cooldown_seconds=120)
    router.register_provider("only", _FakeProvider("whatever"))
    entry = router._find_healthy_provider_entry("")
    assert entry is not None
    assert entry[0] == "only"


def test_find_entry_returns_none_when_all_cooldown() -> None:
    """所有 provider 进入 cooldown 且模型不匹配兜底 → 返回 None。"""
    router = _multi_provider_router()
    for name in ("alpha", "beta"):
        for i in range(3):
            router.mark_failure(name, f"e{i}")
    assert router._find_healthy_provider_entry("unknown-model") is None


# ── resolve 非推理子系统路由 ──────────────────────────────────────────────────


def test_resolve_approval_route_hits_configured_provider() -> None:
    config = ModelsConfig(
        default_model="m1",
        providers=[ProviderConfig(name="guard", models=["guard-model"])],
        routes=[
            ModelRouteConfig(
                provider="guard",
                model="guard-model",
                task_types=["approval"],
            )
        ],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    guard = _FakeProvider("guard-model")
    router.register_provider("guard", guard)

    provider, model = router.resolve("approval")
    assert provider is guard
    assert model == "guard-model"


def test_resolve_compression_route_hits_configured_provider() -> None:
    config = ModelsConfig(
        default_model="m1",
        providers=[ProviderConfig(name="compressor", models=["small-model"])],
        routes=[
            ModelRouteConfig(
                provider="compressor",
                model="small-model",
                task_types=["compression"],
            )
        ],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    comp = _FakeProvider("small-model")
    router.register_provider("compressor", comp)

    provider, model = router.resolve("compression")
    assert provider is comp
    assert model == "small-model"


def test_resolve_falls_back_when_no_route_matches() -> None:
    """无匹配路由 → 返回传入的 fallback_provider/fallback_model（不改变现有部署）。"""
    config = ModelsConfig(default_model="m1", providers=[], routes=[])
    router = ModelRouter(config, cooldown_seconds=120)
    fallback = _FakeProvider("main-model")

    provider, model = router.resolve(
        "approval", fallback_provider=fallback, fallback_model="main-model"
    )
    assert provider is fallback
    assert model == "main-model"


def test_resolve_falls_back_when_route_provider_unavailable() -> None:
    """路由匹配但绑定 provider 处于 cooldown → 落回 fallback。"""
    config = ModelsConfig(
        default_model="m1",
        providers=[ProviderConfig(name="guard", models=["guard-model"])],
        routes=[
            ModelRouteConfig(
                provider="guard", model="guard-model", task_types=["approval"]
            )
        ],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    router.register_provider("guard", _FakeProvider("guard-model"))
    for i in range(3):
        router.mark_failure("guard", f"e{i}")

    fallback = _FakeProvider("main-model")
    provider, model = router.resolve(
        "approval", fallback_provider=fallback, fallback_model="main-model"
    )
    assert provider is fallback
    assert model == "main-model"


# ── _provider_available：cooldown 窗口过后恢复 ────────────────────────────────


def test_provider_available_recovers_after_cooldown_window() -> None:
    config = ModelsConfig(
        default_model="m1",
        providers=[ProviderConfig(name="primary", models=["m1"])],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=120)
    router.register_provider("primary", _FakeProvider("m1"))

    for i in range(3):
        router.mark_failure("primary", f"e{i}")
    assert router._health["primary"].status == HealthStatus.COOLDOWN
    # 窗口内不可用
    assert router._provider_available("primary") is False

    # 推进时间：把 cooldown_until 设为过去（router 用 datetime.now，非 time.monotonic）
    router._health["primary"].cooldown_until = (
        datetime.now(timezone.utc) - timedelta(seconds=1)
    )
    # 第一次访问触发 refresh_if_recovered → HALF_OPEN，并发放探针票
    assert router._provider_available("primary") is True
    assert router._health["primary"].status == HealthStatus.HALF_OPEN
