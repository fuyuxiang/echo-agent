"""backend 候选挑选与探针。"""
from unittest.mock import AsyncMock, MagicMock
import pytest

from echo_agent.agent.loop import (
    _embed_model_identity,
    pick_embed_candidate,
    probe_embed_provider,
)


class _NoEmbed:
    def supports_embed(self): return False


def test_local_backend_never_picks_provider():
    p = MagicMock()
    p.supports_embed = MagicMock(return_value=True)
    cand, model = pick_embed_candidate("local", p, None, "m")
    assert cand is None


def test_auto_picks_main_provider_when_supports():
    p = MagicMock()
    p.supports_embed = MagicMock(return_value=True)
    cand, model = pick_embed_candidate("auto", p, None, "m")
    assert cand is p and model == "m"


def test_auto_routes_via_router_when_main_lacks():
    main = _NoEmbed()
    routed = MagicMock()
    router = MagicMock()
    router.find_embed_provider = MagicMock(return_value=(routed, "routed-model"))
    cand, model = pick_embed_candidate("auto", main, router, "m")
    assert cand is routed and model == "routed-model"


@pytest.mark.asyncio
async def test_probe_returns_dimension_on_success():
    p = MagicMock()
    p.embed = AsyncMock(return_value=[0.1, 0.2, 0.3])
    assert await probe_embed_provider(p, "m", 1.5) == 3


@pytest.mark.asyncio
async def test_probe_returns_zero_on_failure():
    p = MagicMock()
    p.embed = AsyncMock(return_value=None)
    assert await probe_embed_provider(p, "m", 1.5) == 0


@pytest.mark.asyncio
async def test_probe_returns_zero_on_exception():
    p = MagicMock()
    p.embed = AsyncMock(side_effect=RuntimeError("404 no route"))
    assert await probe_embed_provider(p, "m", 1.5) == 0


# ── embed_model_identity：跨包装稳定，避免误判全量陈旧 ───────────────────────

class _Concrete:
    """具体 provider（叶子），无 _inner。"""


class _Wrapper:
    def __init__(self, inner):
        self._inner = inner


def test_model_identity_unwraps_to_concrete_provider():
    concrete = _Concrete()
    bare = _embed_model_identity(concrete, "bge")
    wrapped = _embed_model_identity(_Wrapper(concrete), "bge")
    double = _embed_model_identity(_Wrapper(_Wrapper(concrete)), "bge")
    # 限流/凭据池包装开关不应改变身份 → 不触发无谓全量重嵌。
    assert bare == wrapped == double == "_concrete:bge"


def test_model_identity_defaults_model_name():
    assert _embed_model_identity(_Concrete(), None) == "_concrete:default"


def test_model_identity_handles_self_cycle():
    # _inner 指向自身时不应死循环。
    obj = _Concrete()
    obj._inner = obj
    assert _embed_model_identity(obj, "m") == "_concrete:m"
