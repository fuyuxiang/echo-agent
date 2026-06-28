# tests/test_store_provenance.py
from pathlib import Path

from echo_agent.skills.store import SkillStore, parse_frontmatter


def _make_store(tmp_path: Path) -> SkillStore:
    return SkillStore(user_dir=tmp_path / "skills")


def test_write_provenance_adds_block(tmp_path):
    store = _make_store(tmp_path)
    assert store.create_skill(
        "deploy", "---\nname: deploy\ndescription: d\n---\nbody"
    ) is None

    err = store.write_provenance(
        "deploy",
        source="reviewer",
        created_at="2026-06-28T10:00:00",
        promotion_status="active",
        created_from_session="cli:default",
    )
    assert err is None

    content = store.read_skill("deploy")
    fm, _ = parse_frontmatter(content)
    prov = fm["metadata"]["echo"]["provenance"]
    assert prov["source"] == "reviewer"
    assert prov["promotion_status"] == "active"
    assert prov["created_from_session"] == "cli:default"
    assert prov["created_at"] == "2026-06-28T10:00:00"


def test_write_provenance_preserves_existing_metadata(tmp_path):
    store = _make_store(tmp_path)
    store.create_skill(
        "deploy",
        "---\nname: deploy\ndescription: d\nmetadata:\n  echo:\n    tags: [ops]\n---\nbody",
    )
    store.write_provenance(
        "deploy", source="manual", created_at="t", promotion_status="trusted",
        created_from_session="",
    )
    fm, _ = parse_frontmatter(store.read_skill("deploy"))
    # 既有 tags 不能被 provenance 写入冲掉
    assert fm["metadata"]["echo"]["tags"] == ["ops"]
    assert fm["metadata"]["echo"]["provenance"]["promotion_status"] == "trusted"


def test_write_provenance_unknown_skill_errors(tmp_path):
    store = _make_store(tmp_path)
    err = store.write_provenance(
        "nope", source="reviewer", created_at="t", promotion_status="active",
        created_from_session="",
    )
    assert err is not None
