import pytest

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.admission import SkillAdmission
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_list_staged_and_approve_via_admission(tmp_path, capsys):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    adm = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                         policy="stage_for_review", auto_write_risk="low")
    # 暂存一个高风险 create
    await adm.admit(SkillCandidate(
        operation="create", skill_name="newskill", source="reviewer", risk="high",
        proposed_content="---\nname: newskill\ndescription: d\n---\nbody",
    ))
    staged = await adm.list_staged()
    assert len(staged) == 1
    approved = await adm.approve(staged[0].id)
    assert approved.outcome == "written"
    assert sstore.read_skill("newskill") is not None
    await backend.close()
