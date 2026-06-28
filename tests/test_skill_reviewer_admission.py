# tests/test_skill_reviewer_admission.py
import pytest

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.skills.admission import SkillAdmission
from echo_agent.skills.reviewer import SkillReviewer
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_reviewer_create_routes_through_admission(tmp_path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    adm = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                         policy="stage_for_review", auto_write_risk="low")
    reviewer = SkillReviewer(provider=None, store=sstore, admission=adm,
                             session_key="cli:default", channel="cli")

    # create 是高风险 → 应暂存,不直接落盘
    result = await reviewer._handle_skill_manage({
        "action": "create", "name": "newskill",
        "content": "---\nname: newskill\ndescription: d\n---\nbody",
    })
    assert "staged" in result.lower() or "review" in result.lower()
    assert sstore.read_skill("newskill") is None
    staged = await adm.list_staged()
    assert len(staged) == 1
    assert staged[0].source == "reviewer"
    assert staged[0].created_from_session == "cli:default"
    await backend.close()
