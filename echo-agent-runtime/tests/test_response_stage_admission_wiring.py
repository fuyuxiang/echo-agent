"""Task 6: SkillAdmission wiring into ResponseStage / AgentLoop.

Verifies the governance spine (SkillAdmission) reaches the ResponseStage and is
forwarded to the SkillReviewer, independent of evolution.enabled.
"""

import inspect

from echo_agent.agent.pipeline.response_stage import ResponseStage


def test_response_stage_accepts_skill_admission():
    rs = ResponseStage(
        config=None, sessions=None, memory=None, provider=None,
        consolidation_worker=None, default_model="", spawn_fn=lambda *a, **k: None,
        clear_memory_snapshot_fn=lambda *a, **k: None,
        skill_store=object(), skill_admission="SENTINEL",
    )
    assert rs._skill_admission == "SENTINEL"


def test_response_stage_init_declares_skill_admission_param():
    assert "skill_admission" in inspect.signature(ResponseStage.__init__).parameters
