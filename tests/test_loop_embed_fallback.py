"""嵌入三级瀑布与启动回填接线测试（不依赖真实模型/网络）。"""
from unittest.mock import AsyncMock, MagicMock

from echo_agent.memory.local_embed import LocalEmbedder


class _NoEmbedProvider:
    """模拟 Anthropic 等不支持 embedding 的 provider。"""


def test_local_fallback_selected_when_no_embed_provider():
    """无 embed provider 且 local_embedding_model 非空 → 走本地兜底。"""
    from echo_agent.agent.loop import resolve_embed_fallback

    embed_fn, model_id, local = resolve_embed_fallback(
        embed_provider=None, emb_model=None,
        local_model_name="BAAI/bge-small-zh-v1.5",
    )
    assert embed_fn is not None
    assert model_id == "fastembed:BAAI/bge-small-zh-v1.5"
    assert isinstance(local, LocalEmbedder)


def test_local_fallback_disabled_by_empty_config():
    from echo_agent.agent.loop import resolve_embed_fallback

    embed_fn, model_id, local = resolve_embed_fallback(
        embed_provider=None, emb_model=None, local_model_name="",
    )
    assert embed_fn is None
    assert model_id == ""
    assert local is None


def test_provider_takes_priority_over_local():
    from echo_agent.agent.loop import resolve_embed_fallback

    provider = MagicMock()
    provider.embed = AsyncMock(return_value=[0.1, 0.2])
    embed_fn, model_id, local = resolve_embed_fallback(
        embed_provider=provider, emb_model="text-embedding-3-small",
        local_model_name="BAAI/bge-small-zh-v1.5",
    )
    assert embed_fn is not None
    assert model_id == "magicmock:text-embedding-3-small"
    assert local is None
