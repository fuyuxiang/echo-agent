"""终审 backlog 修复的回归锁：空向量守卫、dangling embedding_id 自愈、wrapper embed 代理。"""
from pathlib import Path
from unittest.mock import MagicMock

import numpy as np
import pytest
import pytest_asyncio

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.memory.vectors import VectorIndex
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.models.rate_limiter import RateLimitedProvider, TokenBucketLimiter
from echo_agent.storage.sqlite import SQLiteBackend

MODEL = "fastembed:BAAI/bge-small-zh-v1.5"


async def fake_embed(text: str) -> list[float]:
    h = abs(hash(text))
    return [float((h >> i) & 0xFF) / 255.0 + 0.01 for i in (0, 8, 16, 24)]


@pytest_asyncio.fixture
async def storage(tmp_path: Path) -> SQLiteBackend:
    backend = SQLiteBackend(tmp_path / "test.db")
    await backend.initialize()
    yield backend
    await backend.close()


# ── 空向量守卫 ───────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_add_rejects_empty_embedding(storage: SQLiteBackend):
    vi = VectorIndex(storage, dimensions=0, model_id=MODEL)
    await vi.initialize()
    assert await vi.add("mem1", []) == ""
    assert vi.dimensions == 0  # 绝不采纳 0 维
    # 后续正常向量不受影响
    assert await vi.add("mem2", [1.0, 0.0, 0.0]) != ""
    assert vi.dimensions == 3


@pytest.mark.asyncio
async def test_initialize_skips_zero_byte_blob(storage: SQLiteBackend):
    """损坏的 0 字节 embedding 行不得炸掉启动，进 stale 等重嵌入。"""
    await storage.store_vector("v_bad", "mem_bad", b"", {}, model=MODEL, dim=4)
    good = np.array([0.0, 1.0, 0.0, 0.0], dtype=np.float32).tobytes()
    await storage.store_vector("v_good", "mem_good", good, {}, model=MODEL, dim=4)
    vi = VectorIndex(storage, dimensions=0, model_id=MODEL)
    await vi.initialize()  # 不抛异常
    assert vi.count == 1
    assert "mem_bad" in vi.stale_source_ids


# ── dangling embedding_id 自愈 ───────────────────────────────────────────────

@pytest.mark.asyncio
async def test_queue_missing_embeds_requeues_dangling(tmp_path: Path, storage: SQLiteBackend):
    """entry 带 embedding_id 但 vectors 表无该行（如删库保 JSON）→ 重新入队。"""
    store = MemoryStore(tmp_path / "memory", storage=storage)
    index = VectorIndex(storage, dimensions=4, model_id=MODEL)
    await index.initialize()
    store.set_vector_index(index)
    store.set_embed_fn(fake_embed)

    e = store.add(MemoryEntry(type=MemoryType.USER, key="k", content="向量库被删的条目"))
    await store.flush_pending_embeds()
    assert store._entries[e.id].embedding_id

    # 模拟 vectors 库被删而 JSON 保留：清空表后重新 initialize
    rows = await storage.load_vectors_all()
    for row in rows:
        await storage.delete_vector(row["id"])
    await index.initialize()
    assert index.count == 0

    queued = store.queue_missing_embeds(index.stale_source_ids)
    assert queued == 1
    await store.flush_pending_embeds()
    assert index.count == 1


# ── wrapper embed 代理 ───────────────────────────────────────────────────────

class _EmbedCapable(LLMProvider):
    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="")

    def get_default_model(self) -> str:
        return "m"

    async def embed(self, text: str, model: str | None = None) -> list[float] | None:
        return [0.1, 0.2]


class _EmbedIncapable(LLMProvider):
    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="")

    def get_default_model(self) -> str:
        return "m"


def test_supports_embed_sees_through_rate_limit_wrapper():
    limiter = TokenBucketLimiter(tokens_per_minute=600)
    wrapped_capable = RateLimitedProvider(_EmbedCapable(), limiter)
    wrapped_incapable = RateLimitedProvider(_EmbedIncapable(), limiter)
    assert wrapped_capable.supports_embed() is True
    assert wrapped_incapable.supports_embed() is False


@pytest.mark.asyncio
async def test_rate_limited_embed_proxies_inner():
    limiter = TokenBucketLimiter(tokens_per_minute=600)
    wrapped = RateLimitedProvider(_EmbedCapable(), limiter)
    assert await wrapped.embed("hi") == [0.1, 0.2]
    wrapped_none = RateLimitedProvider(_EmbedIncapable(), limiter)
    assert await wrapped_none.embed("hi") is None


def test_pooled_provider_supports_embed_delegates():
    from echo_agent.models.providers import _PooledProvider

    pool = MagicMock()
    inner = _EmbedCapable()
    pooled = _PooledProvider(inner, pool, MagicMock())
    assert pooled.supports_embed() is True

    pooled_no = _PooledProvider(_EmbedIncapable(), pool, MagicMock())
    assert pooled_no.supports_embed() is False


def test_router_supports_embed_through_wrapper():
    from echo_agent.models.router import ModelRouter

    limiter = TokenBucketLimiter(tokens_per_minute=600)
    assert ModelRouter._supports_embed(RateLimitedProvider(_EmbedCapable(), limiter)) is True
    assert ModelRouter._supports_embed(_EmbedIncapable()) is False
