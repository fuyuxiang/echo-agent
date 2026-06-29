from pathlib import Path

from echo_agent.knowledge.index import KnowledgeIndex


def _mk_index(tmp_path: Path) -> KnowledgeIndex:
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nthe quick brown fox", encoding="utf-8")
    (docs / "b.md").write_text("# Beta\nlazy dog sleeps", encoding="utf-8")
    return KnowledgeIndex(
        workspace=tmp_path, docs_dir="docs", index_path="idx.json",
        allowed_extensions=[".md"],
    )


def test_v2_index_has_content_hash(tmp_path: Path):
    idx = _mk_index(tmp_path)
    idx.rebuild()
    import json
    data = json.loads((tmp_path / "idx.json").read_text(encoding="utf-8"))
    assert data["format"] == "echo-agent-knowledge-v2"
    assert all("content_hash" in c for c in data["chunks"])


def test_keyword_scores_shared(tmp_path: Path):
    idx = _mk_index(tmp_path)
    idx.rebuild()
    results = idx.search("quick fox", limit=5)
    assert results and "Alpha" in results[0].title


def test_v1_index_auto_upgrades_to_v2(tmp_path: Path):
    import json

    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nthe quick brown fox", encoding="utf-8")
    legacy = {
        "format": "echo-agent-knowledge-v1",
        "generated_at": "2020-01-01T00:00:00",
        "docs_dir": str(docs),
        "chunks": [
            {
                "id": "docs/a.md#0",
                "path": "docs/a.md",
                "title": "Alpha",
                "text": "the quick brown fox",
                "terms": {"quick": 1, "fox": 1},
                "metadata": {},
                "mtime": 0.0,
            }
        ],
    }
    index_path = tmp_path / "idx.json"
    index_path.write_text(json.dumps(legacy, ensure_ascii=False), encoding="utf-8")

    idx = KnowledgeIndex(
        workspace=tmp_path, docs_dir="docs", index_path="idx.json",
        allowed_extensions=[".md"],
    )
    idx.ensure_ready(auto_index=True)

    data = json.loads(index_path.read_text(encoding="utf-8"))
    assert data["format"] == "echo-agent-knowledge-v2"
    assert data["chunks"]
    assert all("content_hash" in c for c in data["chunks"])

