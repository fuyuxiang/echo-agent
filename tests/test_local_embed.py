"""LocalEmbedder 惰性加载、model_id、降级行为测试（不真实下载模型）。"""
import sys
from unittest.mock import MagicMock, patch

import pytest

from echo_agent.memory.local_embed import LocalEmbedder


def test_model_id_format():
    e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
    assert e.model_id == "fastembed:BAAI/bge-small-zh-v1.5"


def test_known_dimensions():
    e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
    assert e.dimensions == 512


def test_unknown_model_dimensions_zero():
    e = LocalEmbedder("some/unknown-model")
    assert e.dimensions == 0


def test_lazy_no_load_on_construct():
    """构造时绝不加载模型（不触发下载）。"""
    e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
    assert e._model is None


@pytest.mark.asyncio
async def test_embed_uses_fastembed_and_caches_model():
    fake_model = MagicMock()
    import numpy as np
    fake_model.embed.return_value = iter([np.array([0.1, 0.2], dtype=np.float32)])
    fake_cls = MagicMock(return_value=fake_model)
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
        vec = await e.embed("你好世界")
        assert vec == pytest.approx([0.1, 0.2], abs=1e-6)
        # 第二次调用复用已加载模型
        fake_model.embed.return_value = iter([np.array([0.3, 0.4], dtype=np.float32)])
        vec2 = await e.embed("again")
        assert vec2 == pytest.approx([0.3, 0.4], abs=1e-6)
        assert fake_cls.call_count == 1


@pytest.mark.asyncio
async def test_embed_returns_none_on_load_failure():
    fake_mod = MagicMock(TextEmbedding=MagicMock(side_effect=RuntimeError("download failed")))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
        assert await e.embed("text") is None
        # 失败后不无限重试加载：第二次直接 None
        assert await e.embed("text") is None
        assert fake_mod.TextEmbedding.call_count == 1


@pytest.mark.asyncio
async def test_embed_returns_none_when_fastembed_missing():
    with patch.dict(sys.modules, {"fastembed": None}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
        assert e.available is False
        assert await e.embed("text") is None


def test_close_is_idempotent_and_releases_pool():
    """close() 回收线程池，可重复调用，且之后 available 池已释放。"""
    e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
    assert e._pool is not None
    e.close()
    assert e._pool is None
    assert e._load_failed is True
    # 二次调用不抛异常（幂等）
    e.close()


@pytest.mark.asyncio
async def test_embed_returns_none_after_close():
    """close() 之后 embed() 安全降级为 None，不会向已关闭的池提交任务。"""
    fake_model = MagicMock()
    import numpy as np
    fake_model.embed.return_value = iter([np.array([0.1, 0.2], dtype=np.float32)])
    fake_mod = MagicMock(TextEmbedding=MagicMock(return_value=fake_model))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5")
        e.close()
        assert await e.embed("text") is None


@pytest.mark.asyncio
async def test_embed_degrades_on_load_hang():
    """下载挂起（不抛异常、只是卡住）也应在超时后降级为 None，而不是永久挂起。"""
    import time

    def _hang(*_a, **_k):
        time.sleep(10)  # 模拟卡死的下载
        return MagicMock()

    fake_mod = MagicMock(TextEmbedding=MagicMock(side_effect=_hang))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        # 极短超时，确认 wait_for 生效并标记失败
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", load_timeout_seconds=0.1)
        assert await e.embed("text") is None
        assert e._load_failed is True
        # 失败后不再触发新的加载
        assert await e.embed("again") is None
