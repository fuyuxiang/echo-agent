"""Evolution cannot shadow protected skills by changing frontmatter names."""

from types import SimpleNamespace

import pytest

from echo_agent.evaluation.dataset import EvalCase, EvalDataset
from echo_agent.evaluation.runner import CaseResult, EvalReport
from echo_agent.evolution.gate import PromotionGate
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.store import SkillStore
from echo_agent.skills.admission import SkillAdmission
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["create", "patch"])
async def test_candidate_cannot_shadow_a_protected_skill_using_frontmatter_alias(tmp_path, operation):
    builtin = tmp_path / "builtin" / "protected"
    builtin.mkdir(parents=True)
    (builtin / "SKILL.md").write_text("---\nname: protected\ndescription: protected instructions\n---\nOriginal")
    skills = SkillStore(user_dir=tmp_path / "skills", builtin_dir=builtin.parent)
    skills.create_skill("custom", "---\nname: custom\ndescription: user skill\n---\nCustom")
    storage = SQLiteBackend(tmp_path / "evolution.db")
    await storage.initialize()
    store = TrajectoryStore(storage)
    await store.init_schema()
    calls = 0

    def factory():
        async def run_dataset(dataset):
            nonlocal calls
            calls += 1
            return EvalReport(results=[CaseResult(case_id=str(i), passed=(i < calls + 1)) for i in range(3)],
                              total_cases=3, passed_cases=calls + 1)
        return SimpleNamespace(run_dataset=run_dataset)

    gate = PromotionGate(eval_runner_factory=factory, eval_dataset_loader=lambda: EvalDataset([EvalCase(id="case")]),
                         skill_store=skills, store=store)
    candidate = SkillCandidate(operation=operation, skill_name="shadow" if operation == "create" else "custom",
                               proposed_content="---\nname: protected\ndescription: shadow\n---\nChanged",
                               proposed_patch_old="name: custom", proposed_patch_new="name: protected")
    await store.save_candidate(candidate)
    try:
        assert skills.is_protected("protected")
        decision = await gate.evaluate(candidate)
        assert not decision.promoted
        assert "name" in decision.reason
        assert "Original" in skills.read_skill("protected")
        assert skills.find_skill_dir("protected") == builtin
        # Background reviewer admission is a second automated writer. It must
        # enforce the same identity rule even outside EvolutionEngine.
        candidate.status = "pending"
        candidate.risk = "low"
        admission = SkillAdmission(skill_store=skills, candidate_store=store,
                                   policy="auto_write", auto_write_risk="low")
        result = await admission.admit(candidate)
        assert result.outcome != "written"
        assert "name" in result.message
        assert skills.find_skill_dir("protected") == builtin
    finally:
        await storage.close()
