# tests/test_candidate_provenance.py
from echo_agent.evolution.types import SkillCandidate


def test_new_provenance_fields_default():
    c = SkillCandidate()
    assert c.source == "evolver"
    assert c.created_by == ""
    assert c.created_from_session == ""
    assert c.channel == ""
    assert c.risk == "high"
    assert c.confidence == 0.0
    assert c.promotion_status == ""


def test_provenance_roundtrip():
    c = SkillCandidate(
        operation="delete",
        skill_name="deploy-helper",
        source="reviewer",
        created_by="reviewer",
        created_from_session="cli:default",
        channel="cli",
        risk="low",
        confidence=0.8,
        promotion_status="active",
    )
    restored = SkillCandidate.from_dict(c.to_dict())
    assert restored.operation == "delete"
    assert restored.source == "reviewer"
    assert restored.created_by == "reviewer"
    assert restored.created_from_session == "cli:default"
    assert restored.channel == "cli"
    assert restored.risk == "low"
    assert restored.confidence == 0.8
    assert restored.promotion_status == "active"


def test_legacy_dict_without_new_fields_loads():
    # 旧 SQLite data JSON 没有新字段,必须回落默认值
    legacy = {"id": "cand_old", "operation": "create", "skill_name": "x"}
    c = SkillCandidate.from_dict(legacy)
    assert c.source == "evolver"
    assert c.risk == "high"
    assert c.confidence == 0.0
