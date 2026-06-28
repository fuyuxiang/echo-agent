from echo_agent.config.schema import SkillsConfig


def test_admission_defaults_are_safe():
    cfg = SkillsConfig()
    assert cfg.admission_policy == "stage_for_review"
    assert cfg.auto_write_risk == "low"


def test_admission_policy_accepts_known_values():
    for v in ("auto_write", "stage_for_review", "manual_only"):
        assert SkillsConfig(admission_policy=v).admission_policy == v
