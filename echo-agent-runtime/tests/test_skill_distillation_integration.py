import pytest
import pytest_asyncio

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.admission import SkillAdmission
from echo_agent.skills.store import SkillStore, parse_frontmatter
from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def adm(tmp_path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    a = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                       policy="stage_for_review", auto_write_risk="low")
    yield a, sstore
    await backend.close()


@pytest.mark.asyncio
async def test_patch_autowrite_records_provenance_in_frontmatter(adm):
    a, sstore = adm
    sstore.create_skill("deploy", "---\nname: deploy\ndescription: d\n---\nold step")
    res = await a.admit(SkillCandidate(
        operation="patch", skill_name="deploy", source="reviewer", risk="low",
        created_from_session="cli:default",
        proposed_patch_old="old step", proposed_patch_new="new step",
    ))
    assert res.outcome == "written"
    fm, _ = parse_frontmatter(sstore.read_skill("deploy"))
    prov = fm["metadata"]["echo"]["provenance"]
    assert prov["source"] == "reviewer"
    assert prov["promotion_status"] == "active"


@pytest.mark.asyncio
async def test_create_stage_then_reject_leaves_nothing_on_disk(adm):
    a, sstore = adm
    res = await a.admit(SkillCandidate(
        operation="create", skill_name="newskill", source="reviewer", risk="high",
        proposed_content="---\nname: newskill\ndescription: d\n---\nbody",
    ))
    assert res.outcome == "staged"
    rejected = await a.reject(res.candidate_id, "not useful")
    assert rejected.outcome == "rejected"
    assert sstore.read_skill("newskill") is None
    # 拒绝后不再出现在 staged 列表
    assert await a.list_staged() == []


@pytest.mark.asyncio
async def test_diff_is_returned_for_review(adm):
    a, sstore = adm
    res = await a.admit(SkillCandidate(
        operation="create", skill_name="x", source="reviewer", risk="high",
        proposed_content="line1\nline2\n",
    ))
    assert res.diff  # 非空 unified diff,供审批展示
    assert "line1" in res.diff
