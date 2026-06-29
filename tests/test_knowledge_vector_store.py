import pytest
from pathlib import Path
from echo_agent.knowledge import vector_store
from echo_agent.knowledge.vector_store import KnowledgeVectorStore


def test_degrades_gracefully_without_faiss(tmp_path: Path, monkeypatch):
    # 模拟 faiss/numpy 缺失:不真正卸载库,只把模块级标志置 False
    monkeypatch.setattr(vector_store, "_HAS_FAISS", False)
    sidecar = tmp_path / "idx.json.vectors.npz"
    store = KnowledgeVectorStore(sidecar, dimensions=3)
    assert store.available is False
    assert store.load() == {}
    assert store.search([0.1, 0.2, 0.3], 1) == []
    # build / save 均为 no-op,不应抛异常
    store.build([("c0", [1.0, 0.0, 0.0])])
    store.save([("c0", [1.0, 0.0, 0.0])])


faiss = pytest.importorskip("faiss")


def test_build_search_and_sidecar_roundtrip(tmp_path: Path):
    sidecar = tmp_path / "idx.json.vectors.npz"
    store = KnowledgeVectorStore(sidecar, dimensions=3)
    ordered = [("c0", [1.0, 0.0, 0.0]), ("c1", [0.0, 1.0, 0.0])]
    store.build(ordered)
    hits = store.search([0.9, 0.1, 0.0], limit=1)
    assert hits and hits[0][0] == "c0"

    store.save(ordered)
    assert sidecar.exists()
    reloaded = KnowledgeVectorStore(sidecar, dimensions=3)
    mapping = reloaded.load()
    assert set(mapping) == {"c0", "c1"}


def test_dimension_mismatch_sidecar_discarded(tmp_path: Path):
    sidecar = tmp_path / "idx.json.vectors.npz"
    store = KnowledgeVectorStore(sidecar, dimensions=3)
    store.save([("c0", [1.0, 0.0, 0.0])])
    # 期望维度变了 → load 应丢弃 sidecar 返回空
    other = KnowledgeVectorStore(sidecar, dimensions=4)
    assert other.load() == {}
