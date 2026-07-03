"""backend 候选挑选与探针。"""
from unittest.mock import AsyncMock, MagicMock
import pytest

from echo_agent.agent.loop import pick_embed_candidate, probe_embed_provider


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
