"""provider embedding 熔断包装：连续3次失败后止损。"""
from unittest.mock import AsyncMock, MagicMock
import pytest

from echo_agent.agent.loop import _ProviderEmbedFn


@pytest.mark.asyncio
async def test_success_returns_vector_and_resets():
    p = MagicMock()
    p.embed = AsyncMock(return_value=[0.1, 0.2])
    fn = _ProviderEmbedFn(p, "m")
    assert await fn("hi") == [0.1, 0.2]
    assert fn.tripped is False


@pytest.mark.asyncio
async def test_trips_after_three_consecutive_failures():
    p = MagicMock()
    p.embed = AsyncMock(return_value=None)  # 失败=返回空
    fn = _ProviderEmbedFn(p, "m")
    for _ in range(3):
        assert await fn("x") == []
    assert fn.tripped is True
    # 熔断后不再调底层
    p.embed.reset_mock()
    assert await fn("x") == []
    p.embed.assert_not_awaited()


@pytest.mark.asyncio
async def test_success_midway_resets_counter():
    p = MagicMock()
    p.embed = AsyncMock(side_effect=[None, None, [1.0], None, None])
    fn = _ProviderEmbedFn(p, "m")
    await fn("a"); await fn("b")          # 2 fails
    assert await fn("c") == [1.0]         # success resets
    await fn("d"); await fn("e")          # 2 fails again
    assert fn.tripped is False            # never reached 3 consecutive


@pytest.mark.asyncio
async def test_exception_counts_as_failure():
    p = MagicMock()
    p.embed = AsyncMock(side_effect=RuntimeError("404"))
    fn = _ProviderEmbedFn(p, "m")
    for _ in range(3):
        assert await fn("x") == []
    assert fn.tripped is True
