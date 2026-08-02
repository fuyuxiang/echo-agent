"""Knowledge index durability regressions.

Three defects this pins, all reproduced against the pre-fix code:

1. ``_is_stale()`` compared "is any document newer than the index file", which
   cannot see a deletion or a rename — every surviving file is older than the
   index, so it reported fresh and kept serving chunks from documents that no
   longer existed. The ghost hits survived restarts.
2. ``_save()`` used a plain ``write_text``, so a crash mid-write left a
   truncated index.
3. ``load()`` let a JSONDecodeError escape. ``ensure_ready()`` runs inside
   ``AgentLoop.__init__``, so one corrupt file stopped the agent from starting.

The index is a derived artifact: the documents are the authority, so every
failure here must recover by rebuilding rather than by failing.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import pytest

from echo_agent.knowledge.index import KnowledgeIndex


def _index(tmp_path: Path) -> KnowledgeIndex:
    return KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json")


def _seed(tmp_path: Path, **docs: str) -> Path:
    docs_dir = tmp_path / "docs"
    docs_dir.mkdir(exist_ok=True)
    for name, body in docs.items():
        (docs_dir / f"{name}.md").write_text(body, encoding="utf-8")
    return docs_dir


def _mark_index_fresh(tmp_path: Path) -> None:
    """Make the index file look newer than every document.

    Without this the old mtime heuristic would call the index stale for an
    unrelated reason, and a test meant to exercise deletion detection would pass
    on a coincidence.
    """
    future = time.time() + 10
    os.utime(tmp_path / "idx.json", (future, future))


# ── deletion / rename detection ──────────────────────────────────────────────


def test_delete_marks_index_stale(tmp_path):
    docs = _seed(tmp_path, a="# A\nalpha content", b="# B\nbeta content")
    idx = _index(tmp_path)
    idx.ensure_ready()
    assert idx.doc_count == 2

    (docs / "b.md").unlink()
    _mark_index_fresh(tmp_path)

    assert idx._is_stale() is True


def test_deleted_document_gone_after_restart_no_ghost_hits(tmp_path):
    """The defect's user-visible shape: a deleted document kept answering
    queries, across restarts, because the index never noticed it was gone."""
    docs = _seed(tmp_path, a="# A\nalpha content", b="# B\nbeta content")
    _index(tmp_path).ensure_ready()

    (docs / "b.md").unlink()
    _mark_index_fresh(tmp_path)

    restarted = _index(tmp_path)
    restarted.ensure_ready()

    assert restarted.doc_count == 1
    assert {c["path"] for c in restarted._chunks} == {"docs/a.md"}
    assert [r.path for r in restarted.search("beta", limit=5)] == []


def test_rename_marks_index_stale(tmp_path):
    """A rename changes no content and no mtime — only the path set."""
    docs = _seed(tmp_path, a="# A\nalpha content")
    idx = _index(tmp_path)
    idx.ensure_ready()

    (docs / "a.md").rename(docs / "renamed.md")
    _mark_index_fresh(tmp_path)

    assert idx._is_stale() is True


def test_in_place_edit_still_detected(tmp_path):
    """The case the old heuristic did handle must keep working."""
    docs = _seed(tmp_path, a="# A\nalpha content")
    idx = _index(tmp_path)
    idx.ensure_ready()

    (docs / "a.md").write_text("# A\ncompletely different body", encoding="utf-8")
    assert idx._is_stale() is True


def test_unchanged_corpus_is_not_stale(tmp_path):
    """Freshness must not flap: an untouched corpus triggers no rebuild."""
    _seed(tmp_path, a="# A\nalpha content", b="# B\nbeta content")
    idx = _index(tmp_path)
    idx.ensure_ready()

    assert idx._is_stale() is False


# ── corruption recovery ──────────────────────────────────────────────────────


def test_truncated_index_rebuilds_instead_of_raising(tmp_path):
    """ensure_ready() runs in AgentLoop.__init__, so a corrupt index used to
    stop the whole agent from starting with a bare JSONDecodeError."""
    _seed(tmp_path, a="# A\nalpha content")
    _index(tmp_path).ensure_ready()

    path = tmp_path / "idx.json"
    raw = path.read_text(encoding="utf-8")
    path.write_text(raw[: len(raw) // 2], encoding="utf-8")
    _mark_index_fresh(tmp_path)

    recovered = _index(tmp_path)
    recovered.ensure_ready()  # must not raise

    assert recovered.chunk_count == 1
    assert [r.path for r in recovered.search("alpha", limit=3)] == ["docs/a.md"]


def test_corrupt_index_is_quarantined_on_load(tmp_path):
    """load() reached directly (auto_index=False path, or a mid-run reload)
    quarantines the unreadable file rather than propagating."""
    _seed(tmp_path, a="# A\nalpha content")
    _index(tmp_path).ensure_ready()

    path = tmp_path / "idx.json"
    path.write_text("{not json at all", encoding="utf-8")

    idx = _index(tmp_path)
    idx.load()  # must not raise

    assert idx._needs_rebuild is True
    assert list(tmp_path.glob("idx.json.corrupt.*")), "broken index kept as evidence"


def test_non_object_index_root_is_treated_as_corrupt(tmp_path):
    """A well-formed JSON document of the wrong shape (list, string) must not
    slip past the parse into ``.get()`` on a non-dict."""
    _seed(tmp_path, a="# A\nalpha content")
    _index(tmp_path).ensure_ready()
    (tmp_path / "idx.json").write_text("[1, 2, 3]", encoding="utf-8")

    idx = _index(tmp_path)
    idx.load()

    assert idx._needs_rebuild is True
    assert idx.chunk_count == 0


# ── atomic write ─────────────────────────────────────────────────────────────


def test_save_leaves_no_temp_file(tmp_path):
    _seed(tmp_path, a="# A\nalpha content")
    _index(tmp_path).ensure_ready()

    assert list(tmp_path.glob(".*.tmp")) == []
    assert (tmp_path / "idx.json").exists()


def test_failed_save_does_not_clobber_existing_index(tmp_path):
    """A write that dies mid-flight must leave the previous index intact —
    the whole point of temp + replace over write_text."""
    _seed(tmp_path, a="# A\nalpha content")
    idx = _index(tmp_path)
    idx.ensure_ready()
    good = (tmp_path / "idx.json").read_text(encoding="utf-8")

    class Boom(RuntimeError):
        pass

    original = json.dump

    def exploding_dump(*args, **kwargs):
        original(*args, **kwargs)
        raise Boom("disk full")

    json.dump = exploding_dump
    try:
        with pytest.raises(Boom):
            idx._save()
    finally:
        json.dump = original

    assert (tmp_path / "idx.json").read_text(encoding="utf-8") == good
    assert list(tmp_path.glob(".*.tmp")) == []


# ── format migration ─────────────────────────────────────────────────────────


def test_v2_index_without_manifest_upgrades(tmp_path):
    """Existing installs carry a v2 index with no manifest. It must load, be
    flagged for rebuild, and come back as v3 with a manifest — not be read as
    'the corpus is empty', which would look like every document was deleted."""
    _seed(tmp_path, a="# A\nalpha content")
    legacy = {
        "format": "echo-agent-knowledge-v2",
        "chunks": [{
            "id": "docs/a.md#0", "path": "docs/a.md", "title": "A",
            "text": "alpha content", "terms": {"alpha": 1},
            "content_hash": "stale", "metadata": {}, "mtime": 0,
        }],
    }
    (tmp_path / "idx.json").write_text(json.dumps(legacy), encoding="utf-8")
    _mark_index_fresh(tmp_path)

    idx = _index(tmp_path)
    idx.ensure_ready()

    written = json.loads((tmp_path / "idx.json").read_text(encoding="utf-8"))
    assert written["format"] == "echo-agent-knowledge-v3"
    assert written["manifest"], "upgraded index must carry a manifest"
    assert idx._is_stale() is False
