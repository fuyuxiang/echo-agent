import pytest
from pathlib import Path
from echo_agent.knowledge.vector_store import KnowledgeVectorStore

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
