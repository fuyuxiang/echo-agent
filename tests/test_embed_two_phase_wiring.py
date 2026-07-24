"""两阶段接线：__init__ 只挑候选不定案，start() 探针后定案并构造 VectorIndex。

覆盖 auto 探针成功走 provider、auto 探针失败静默回退 fastembed、
local 免探针、provider 探针失败抛错四条路径，外加 start 前不构造索引。
不依赖真实模型/网络：provider 用 MagicMock，storage 用内存态 SQLite。
"""
from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

import echo_agent.agent.loop as loop_mod
from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.fixture
def agent_loop_factory(tmp_path, monkeypatch):
    """构造最小 AgentLoop 的工厂：可控 embedding_backend 与 provider 返回向量。

    - provider: MagicMock（supports_embed 恒 True、embed 返回 provider_embeds）；
      用 MagicMock 是为了让 provider 型 model_id 前缀落在 'magicmock:'。
    - storage: 内存态 SQLite（首次查询时惰性连接，无需预先 initialize）。
    - _probe_spy: 包一层计数的 probe_embed_provider，验证 local 免探针。
    """

    def _make(embedding_backend="auto", provider_embeds=None):
        config = load_config(overrides={"workspace": str(tmp_path)})
        config.memory.enabled = True
        config.memory.vector_enabled = True
        config.memory.embedding_backend = embedding_backend
        # 关掉无关子系统，缩小构造面（探测特性只关心 memory/embed 链路）。
        config.knowledge.enabled = False
        config.planning.enabled = False
        config.multi_agent.enabled = False
        config.observability.otel_enabled = False
        config.observability.trace_enabled = False

        # spec=LLMProvider 关键：裸 MagicMock 会对任意属性自动造子 mock，
        # discover_tools 里 `_unwrap_provider` 的 `while hasattr(p, "_inner")`
        # 会因此无限递归。限定 spec 后不存在的属性抛 AttributeError，循环即止。
        provider = MagicMock(spec=LLMProvider)
        provider.supports_embed = MagicMock(return_value=True)
        provider.embed = AsyncMock(return_value=provider_embeds)
        provider.get_default_model = MagicMock(return_value="stub")
        provider.chat_with_retry = AsyncMock()

        storage = SQLiteBackend(tmp_path / f"mem_{embedding_backend}.db")

        loop = AgentLoop(
            bus=MessageBus(),
            config=config,
            provider=provider,
            workspace=tmp_path,
            storage=storage,
        )

        # 包一层计数 spy，委托回真实探针；start() 内按模块全局名解析，故可被替换。
        spy = AsyncMock(side_effect=loop_mod.probe_embed_provider)
        monkeypatch.setattr(loop_mod, "probe_embed_provider", spy)
        loop._probe_spy = spy
        return loop

    return _make


@pytest.mark.asyncio
async def test_vector_index_none_before_start(monkeypatch, agent_loop_factory):
    loop = agent_loop_factory(embedding_backend="auto")
    assert loop._vector_index is None      # 阶段 A 不构造
    assert loop._embed_fn is None


@pytest.mark.asyncio
async def test_auto_probe_success_uses_provider(agent_loop_factory):
    loop = agent_loop_factory(embedding_backend="auto", provider_embeds=[0.1, 0.2, 0.3])
    await loop.start()
    assert loop._vector_index is not None
    assert loop._vector_index.dimensions == 3
    assert loop._embed_model_id.startswith(("magicmock", "openai"))  # provider 型


@pytest.mark.asyncio
async def test_auto_probe_failure_falls_back_to_fastembed(agent_loop_factory):
    with patch("echo_agent.memory.local_embed.LocalEmbedder.available",
               new_callable=lambda: property(lambda self: True)):
        loop = agent_loop_factory(embedding_backend="auto", provider_embeds=None)  # 探针失败
        await loop.start()
    assert loop._embed_model_id.startswith("fastembed:")


@pytest.mark.asyncio
async def test_local_backend_skips_probe(agent_loop_factory):
    loop = agent_loop_factory(embedding_backend="local", provider_embeds=[0.1])
    probe = loop._probe_spy  # factory 注入的探针调用记录
    await loop.start()
    assert loop._embed_model_id.startswith("fastembed:")
    assert probe.call_count == 0


@pytest.mark.asyncio
async def test_provider_backend_probe_failure_raises(agent_loop_factory):
    loop = agent_loop_factory(embedding_backend="provider", provider_embeds=None)
    with pytest.raises(RuntimeError, match="embedding"):
        await loop.start()


@pytest.mark.asyncio
async def test_vector_disabled_still_wires_keyword_consumers(tmp_path):
    """vector_enabled=False（但 memory.enabled=True）时，消费者仍须按原语义接线：
    HybridRetriever 以关键词模式建成、矛盾检测器建成，仅 VectorIndex 缺席。

    覆盖两阶段改造相对改造前的回归——早退曾让 _wire_vector_consumers 永不执行，
    导致关键词检索/矛盾工具/预取在该配置下一起静默失效。
    """
    config = load_config(overrides={"workspace": str(tmp_path)})
    config.memory.enabled = True
    config.memory.vector_enabled = False        # 关向量，仅关键词检索
    config.memory.contradiction_detection = True
    config.memory.reflection_enabled = True
    config.knowledge.enabled = False
    config.planning.enabled = False
    config.multi_agent.enabled = False
    config.observability.otel_enabled = False
    config.observability.trace_enabled = False

    provider = MagicMock(spec=LLMProvider)
    provider.supports_embed = MagicMock(return_value=True)
    provider.embed = AsyncMock(return_value=[0.1, 0.2, 0.3])
    provider.get_default_model = MagicMock(return_value="stub")
    provider.chat_with_retry = AsyncMock()

    storage = SQLiteBackend(tmp_path / "mem_vec_off.db")

    loop = AgentLoop(
        bus=MessageBus(),
        config=config,
        provider=provider,
        workspace=tmp_path,
        storage=storage,
    )
    await loop.start()

    assert loop._hybrid_retriever is not None       # 关键词模式仍在
    assert loop._contradiction_detector is not None  # 矛盾检测仍建
    assert loop._prefetcher is not None              # 预取仍接线
    assert loop._vector_index is None                # 但不建向量索引
    assert loop._embed_fn is None


@pytest.mark.asyncio
async def test_rerank_enabled_by_default_builds_reranker(agent_loop_factory):
    """默认 rerank_enabled=True:构造 reranker 并接入 rerank_fn(懒加载,构造不下载)。"""
    loop = agent_loop_factory(embedding_backend="local", provider_embeds=[0.1])
    await loop.start()
    assert loop._reranker is not None
    assert loop._hybrid_retriever._rerank_fn is not None
    loop._reranker.close()


@pytest.mark.asyncio
async def test_rerank_can_be_disabled(tmp_path):
    """显式关闭 → 不构造 reranker,检索器无 rerank_fn。"""
    config = load_config(overrides={"workspace": str(tmp_path)})
    config.memory.enabled = True
    config.memory.vector_enabled = False
    config.memory.rerank_enabled = False
    config.knowledge.enabled = False
    config.planning.enabled = False
    config.multi_agent.enabled = False
    config.observability.otel_enabled = False
    config.observability.trace_enabled = False

    provider = MagicMock(spec=LLMProvider)
    provider.supports_embed = MagicMock(return_value=True)
    provider.embed = AsyncMock(return_value=[0.1])
    provider.get_default_model = MagicMock(return_value="stub")
    provider.chat_with_retry = AsyncMock()

    loop = AgentLoop(
        bus=MessageBus(), config=config, provider=provider,
        workspace=tmp_path, storage=SQLiteBackend(tmp_path / "mem_norr.db"),
    )
    await loop.start()
    assert loop._reranker is None
    assert loop._hybrid_retriever._rerank_fn is None


@pytest.mark.asyncio
async def test_rerank_enabled_builds_reranker_and_wires_fn(tmp_path):
    """rerank_enabled=True:构造 LocalReranker 并把 rerank_fn/top_k/min_score 接入检索器。"""
    config = load_config(overrides={"workspace": str(tmp_path)})
    config.memory.enabled = True
    config.memory.vector_enabled = False
    config.memory.rerank_enabled = True
    config.memory.rerank_top_k = 15
    config.memory.rerank_min_score = 0.4
    config.knowledge.enabled = False
    config.planning.enabled = False
    config.multi_agent.enabled = False
    config.observability.otel_enabled = False
    config.observability.trace_enabled = False

    provider = MagicMock(spec=LLMProvider)
    provider.supports_embed = MagicMock(return_value=True)
    provider.embed = AsyncMock(return_value=[0.1])
    provider.get_default_model = MagicMock(return_value="stub")
    provider.chat_with_retry = AsyncMock()

    loop = AgentLoop(
        bus=MessageBus(), config=config, provider=provider,
        workspace=tmp_path, storage=SQLiteBackend(tmp_path / "mem_rr.db"),
    )
    await loop.start()

    assert loop._reranker is not None
    r = loop._hybrid_retriever
    assert r._rerank_fn is not None
    assert r._rerank_top_k == 15
    assert r._rerank_min_score == 0.4
    # 清理专用线程池
    loop._reranker.close()
