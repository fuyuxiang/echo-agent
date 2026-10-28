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


@pytest.mark.asyncio
@pytest.mark.parametrize("action", ["write_file", "remove_file"])
async def test_reviewer_supporting_file_blocked_under_admission(tmp_path, action):
    """admission 激活时,背景 reviewer 不得绕过治理层写/删支持文件。

    write_file/remove_file 落盘的 scripts/ 等文件未来会被技能系统加载执行,
    必须经显式/人工路径,而非背景 LLM 自动写入。"""
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    # 先经审批落一个真实技能,使 write_file 的目标技能存在(隔离掉"技能不存在"这条路径)
    sstore.create_skill("victim", "---\nname: victim\ndescription: d\n---\nbody")
    adm = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                         policy="auto_write", auto_write_risk="high")
    reviewer = SkillReviewer(provider=None, store=sstore, admission=adm,
                             session_key="cli:default", channel="cli")

    params = {"action": action, "name": "victim", "file_path": "scripts/evil.py"}
    if action == "write_file":
        params["content"] = "import os; os.system('rm -rf /')"
    result = await reviewer._handle_skill_manage(params)

    # 应被拒,且不落盘
    assert result.lower().startswith("error") or "not allowed" in result.lower()
    assert not (tmp_path / "skills" / "victim" / "scripts" / "evil.py").exists()
    # 也不得在审计/候选表里留下任何记录
    assert await adm.list_staged() == []
    await backend.close()


@pytest.mark.asyncio
async def test_reviewer_edit_full_replace_through_admission(tmp_path):
    """admission 下 edit 必须保留整篇 replace 语义:基于当前 SKILL.md 生成明确 patch,
    审批后内容真正被替换,而非生成空 old/new 的 no-op patch。"""
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    sstore.create_skill("greet", "---\nname: greet\ndescription: old\n---\nold body")
    # auto_write + low risk(edit 映射为 low)→ 应直接写盘
    adm = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                         policy="auto_write", auto_write_risk="low")
    reviewer = SkillReviewer(provider=None, store=sstore, admission=adm,
                             session_key="cli:default", channel="cli")

    new_full = "---\nname: greet\ndescription: new\n---\nnew body"
    result = await reviewer._handle_skill_manage({
        "action": "edit", "name": "greet", "content": new_full,
    })

    assert "written" in result.lower() or "applied" in result.lower()
    # 内容真正被整篇替换(provenance 镜像会再往 frontmatter 注入 metadata.echo,
    # 那是 create/patch 既有行为,故只断言正文与描述确被换新,而非逐字节相等)
    updated = sstore.read_skill("greet")
    assert "new body" in updated
    assert "description: new" in updated
    assert "old body" not in updated
    await backend.close()
