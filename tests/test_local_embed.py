"""LocalEmbedder 惰性加载、model_id、降级行为测试（不真实下载模型）。"""
import asyncio
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
async def test_hf_endpoint_injected_before_load(monkeypatch):
    """配置的镜像地址在加载模型前 setdefault 到 HF_ENDPOINT。"""
    monkeypatch.delenv("HF_ENDPOINT", raising=False)
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_mod = MagicMock(TextEmbedding=MagicMock(return_value=fake_model))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        import os
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", hf_endpoint="https://hf-mirror.com")
        await e.embed("text")
        assert os.environ.get("HF_ENDPOINT") == "https://hf-mirror.com"


@pytest.mark.asyncio
async def test_hf_endpoint_does_not_override_env(monkeypatch):
    """操作者已设 HF_ENDPOINT 时，配置默认值不覆盖(setdefault 语义)。"""
    monkeypatch.setenv("HF_ENDPOINT", "https://custom.example")
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_mod = MagicMock(TextEmbedding=MagicMock(return_value=fake_model))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        import os
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", hf_endpoint="https://hf-mirror.com")
        await e.embed("text")
        assert os.environ.get("HF_ENDPOINT") == "https://custom.example"


@pytest.mark.asyncio
async def test_empty_hf_endpoint_leaves_env_untouched(monkeypatch):
    """空串 endpoint 不写入 HF_ENDPOINT。"""
    monkeypatch.delenv("HF_ENDPOINT", raising=False)
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_mod = MagicMock(TextEmbedding=MagicMock(return_value=fake_model))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        import os
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", hf_endpoint="")
        await e.embed("text")
        assert "HF_ENDPOINT" not in os.environ


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
    assert e._closed is True
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
async def test_embed_degrades_on_load_hang_then_recovers():
    """下载慢（超过单轮预算）时本轮降级为 None，但后台加载继续，完成后被后续调用透明拾取。"""
    import threading

    # The load blocks on an Event until we release it, simulating a slow download
    # that exceeds a single message's budget. The first embed() waits only the
    # tiny per-call budget and degrades to None WITHOUT killing the background
    # load. After we release the event the model finishes loading and a later
    # embed() picks it up — the download is never thrown away and restarted.
    started = threading.Event()
    release = threading.Event()

    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.5, 0.6], dtype=np.float32)])

    def _slow_load(*_a, **_k):
        started.set()
        release.wait(timeout=5)  # bounded so it can't wedge CI
        return fake_model

    fake_mod = MagicMock(TextEmbedding=MagicMock(side_effect=_slow_load))
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        # Tiny per-call budget: the first turn degrades while the load runs on.
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", load_timeout_seconds=0.05)
        assert await e.embed("text") is None
        assert e._closed is False  # not permanently disabled
        assert started.is_set()    # background load did start
        # Let the slow load finish, then a later call transparently uses it.
        release.set()
        for _ in range(200):  # poll until the background future resolves
            vec = await e.embed("again")
            if vec is not None:
                break
            await asyncio.sleep(0.01)
        assert vec == pytest.approx([0.5, 0.6], abs=1e-6)
        # The single background load was reused, not restarted per message.
        assert fake_mod.TextEmbedding.call_count == 1


@pytest.mark.asyncio
async def test_load_failure_retries_after_backoff():
    """加载失败后进入退避窗口，不逐条消息重试；退避到期后重新尝试并可成功。"""
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    # First construction attempt fails, second succeeds.
    fake_cls = MagicMock(side_effect=[RuntimeError("net down"), fake_model])
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder(
            "BAAI/bge-small-zh-v1.5", load_timeout_seconds=1.0,
            max_load_attempts=5, retry_backoff_seconds=999.0,
        )
        assert await e.embed("text") is None
        assert fake_cls.call_count == 1
        # Within the backoff window: no new load attempt.
        assert await e.embed("text") is None
        assert fake_cls.call_count == 1
        # Force the backoff to expire, then the next call retries and succeeds.
        e._next_retry_at = 0.0
        vec = await e.embed("text")
        assert vec == pytest.approx([0.1], abs=1e-6)
        assert fake_cls.call_count == 2


@pytest.mark.asyncio
async def test_load_stops_after_max_attempts():
    """连续失败达到上限后本进程不再尝试加载（保持关键词检索直到重启）。"""
    fake_cls = MagicMock(side_effect=RuntimeError("net down"))
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder(
            "BAAI/bge-small-zh-v1.5", load_timeout_seconds=1.0,
            max_load_attempts=2, retry_backoff_seconds=0.0,
        )
        assert await e.embed("text") is None
        assert await e.embed("text") is None
        # Exhausted: further calls never construct again.
        assert await e.embed("text") is None
        assert fake_cls.call_count == 2


@pytest.mark.asyncio
async def test_cache_dir_passed_to_fastembed():
    """配置的缓存目录透传给 fastembed，使安装期预取与运行期共用同一目录。"""
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_cls = MagicMock(return_value=fake_model)
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir="/tmp/echo-models")
        await e.embed("text")
        _, kwargs = fake_cls.call_args
        assert kwargs.get("cache_dir") == "/tmp/echo-models"


@pytest.mark.asyncio
async def test_no_cache_dir_omits_kwarg():
    """未配置缓存目录时不传 cache_dir，保留 fastembed 默认行为。"""
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_cls = MagicMock(return_value=fake_model)
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir="")
        await e.embed("text")
        _, kwargs = fake_cls.call_args
        assert "cache_dir" not in kwargs


@pytest.mark.asyncio
async def test_ready_cache_pins_local_files_only():
    """release 包已就绪时，强制 fastembed 走离线本地缓存，不抢先联网。

    否则 fastembed 0.8 会先探 HF 源，CN 网络下拉 onnx 走 Xet(xethub)撞 401，
    异常逃出其自身 GCS 兜底，白白晾着已就绪的本地包降级为关键词检索。"""
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_cls = MagicMock(return_value=fake_model)
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir="/tmp/echo-models")
        with patch.object(e, "_fetch_release_package", return_value=True):
            await e.embed("text")
        _, kwargs = fake_cls.call_args
        assert kwargs.get("local_files_only") is True


@pytest.mark.asyncio
async def test_unready_cache_stays_online():
    """缓存未就绪(冷启动首下)时保持联网，让 fastembed 的 hf/gcs 源仍能自愈。"""
    import numpy as np
    fake_model = MagicMock()
    fake_model.embed.return_value = iter([np.array([0.1], dtype=np.float32)])
    fake_cls = MagicMock(return_value=fake_model)
    fake_mod = MagicMock(TextEmbedding=fake_cls)
    with patch.dict(sys.modules, {"fastembed": fake_mod}):
        e = LocalEmbedder("BAAI/bge-small-zh-v1.5", cache_dir="/tmp/echo-models")
        with patch.object(e, "_fetch_release_package", return_value=False):
            await e.embed("text")
        _, kwargs = fake_cls.call_args
        assert "local_files_only" not in kwargs
