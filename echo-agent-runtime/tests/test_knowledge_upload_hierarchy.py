from pathlib import Path

from echo_agent.gateway.api.knowledge import _safe_relative_dest


def test_safe_relative_dest_keeps_subdirs(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    dest = _safe_relative_dest(docs, "reports/2026/q1.pdf")
    assert dest == (docs / "reports/2026/q1.pdf").resolve()


def test_safe_relative_dest_rejects_traversal(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    assert _safe_relative_dest(docs, "../evil.txt") is None
    assert _safe_relative_dest(docs, "/etc/passwd") is None


def test_safe_relative_dest_flattens_when_no_relpath(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    dest = _safe_relative_dest(docs, "a.txt")
    assert dest == (docs / "a.txt").resolve()


def test_safe_relative_dest_rejects_sibling_prefix_escape(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    sibling = tmp_path / "docs-evil"
    sibling.mkdir()
    # A plain name stays inside docs.
    assert _safe_relative_dest(docs, "x") == (docs / "x").resolve()
    # Cannot escape into the sibling dir whose name shares the docs prefix.
    assert _safe_relative_dest(docs, "../docs-evil/x") is None


def test_safe_relative_dest_rejects_mid_segment_traversal(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    assert _safe_relative_dest(docs, "a/../../etc/x") is None


def test_safe_relative_dest_rejects_dot_root(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    assert _safe_relative_dest(docs, ".") is None
