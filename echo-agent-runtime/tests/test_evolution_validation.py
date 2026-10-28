"""Tests for evolution configuration validation."""


from echo_agent.config.schema import EvolutionConfig
from echo_agent.evolution.validation import validate_evolution_config


class TestValidateEvolutionConfig:
    def test_safe_defaults_produce_no_warnings(self):
        config = EvolutionConfig()
        warnings = validate_evolution_config(config)
        assert len(warnings) == 0

    def test_dangerous_auto_promote_combination(self):
        config = EvolutionConfig(
            auto_promote=True,
            require_strict_improvement=False,
            regression_threshold=0.15,
        )
        warnings = validate_evolution_config(config)
        errors = [w for w in warnings if w.level == "error"]
        assert len(errors) == 1
        assert "silent quality degradation" in errors[0].message

    def test_short_cooldown_with_auto_promote(self):
        config = EvolutionConfig(
            auto_promote=True,
            cooldown_seconds_after_promote=1800,
        )
        warnings = validate_evolution_config(config)
        warns = [w for w in warnings if w.level == "warning"]
        assert any("rapid churn" in w.message for w in warns)

    def test_review_required_with_auto_promote_warns(self):
        config = EvolutionConfig(
            auto_promote=True,
            candidate_review_required=True,
        )
        warnings = validate_evolution_config(config)
        assert any("contradictory" in w.message for w in warnings)
        assert all(w.level == "warning" for w in warnings)

    def test_high_regression_threshold_warns(self):
        config = EvolutionConfig(
            regression_threshold=0.25,
        )
        warnings = validate_evolution_config(config)
        assert any("unusually permissive" in w.message for w in warnings)

    def test_high_candidates_per_run_warns(self):
        config = EvolutionConfig(
            max_candidates_per_run=15,
        )
        warnings = validate_evolution_config(config)
        assert any("resource contention" in w.message for w in warnings)

    def test_no_warning_for_manual_mode(self):
        config = EvolutionConfig(
            auto_promote=False,
            require_strict_improvement=False,
            regression_threshold=0.2,
        )
        warnings = validate_evolution_config(config)
        errors = [w for w in warnings if w.level == "error"]
        assert len(errors) == 0
