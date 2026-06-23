"""inconclusive 信号在 metric/result/report 间的传播。"""
from __future__ import annotations

import pytest

from echo_agent.evaluation.metrics import (
    MetricResult,
    not_contains,
    forbidden_tools_check,
)
from echo_agent.evaluation.runner import CaseResult, EvalReport
from echo_agent.evaluation.semantic_metrics import semantic_quality


def test_metric_result_inconclusive_defaults_false():
    m = MetricResult(name="x", score=1.0, passed=True)
    assert m.inconclusive is False


def test_metric_result_inconclusive_settable():
    m = MetricResult(name="x", score=0.5, passed=False, inconclusive=True)
    assert m.inconclusive is True


def test_not_contains_passes_when_absent():
    r = not_contains(["password", "secret"], "here is a safe answer")
    assert r.passed is True
    assert r.score == 1.0


def test_not_contains_fails_when_any_present():
    r = not_contains(["password"], "the password is 1234")
    assert r.passed is False
    assert r.score == 0.0


def test_not_contains_empty_list_passes():
    r = not_contains([], "anything")
    assert r.passed is True


def test_forbidden_tools_passes_when_none_used():
    r = forbidden_tools_check(["exec", "process"], ["search", "read"])
    assert r.passed is True


def test_forbidden_tools_fails_when_any_used():
    r = forbidden_tools_check(["exec"], ["search", "exec"])
    assert r.passed is False


def test_forbidden_tools_empty_list_passes():
    r = forbidden_tools_check([], ["exec"])
    assert r.passed is True


def test_not_contains_ignores_empty_string():
    r = not_contains([""], "anything at all")
    assert r.passed is True


def test_not_contains_dedups_violations():
    r = not_contains(["pw", "pw"], "my pw here")
    assert r.details["violations"] == ["pw"]


class _RaisingProvider:
    async def chat_with_retry(self, **kwargs):
        raise RuntimeError("network down")


class _ErrorResp:
    finish_reason = "error"
    content = "boom"


class _ErrorProvider:
    async def chat_with_retry(self, **kwargs):
        return _ErrorResp()


class _BadJsonResp:
    finish_reason = "stop"
    content = "not json at all"


class _BadJsonProvider:
    async def chat_with_retry(self, **kwargs):
        return _BadJsonResp()


@pytest.mark.asyncio
async def test_semantic_quality_call_exception_is_inconclusive():
    r = await semantic_quality("ref", "act", _RaisingProvider())
    assert r.inconclusive is True
    assert r.passed is False


@pytest.mark.asyncio
async def test_semantic_quality_error_response_is_inconclusive():
    r = await semantic_quality("ref", "act", _ErrorProvider())
    assert r.inconclusive is True


@pytest.mark.asyncio
async def test_semantic_quality_unparseable_is_inconclusive():
    r = await semantic_quality("ref", "act", _BadJsonProvider())
    assert r.inconclusive is True


def _case(score, inconclusive=False, category="", passed=True):
    m = MetricResult(name="x", score=score, passed=passed, inconclusive=inconclusive)
    return CaseResult(case_id="c", passed=passed, category=category, metrics=[m])


def test_case_result_inconclusive_derived_from_metrics():
    assert _case(0.5, inconclusive=True).inconclusive is True
    assert _case(1.0, inconclusive=False).inconclusive is False


def test_report_counts_inconclusive_cases():
    rep = EvalReport(
        results=[_case(1.0), _case(0.5, inconclusive=True), _case(1.0)],
        total_cases=3, passed_cases=2,
    )
    assert rep.inconclusive_cases == 1


def test_avg_score_excludes_inconclusive():
    rep = EvalReport(
        results=[_case(1.0), _case(1.0), _case(0.5, inconclusive=True)],
        total_cases=3, passed_cases=2,
    )
    assert rep.avg_score == 1.0


def test_avg_score_all_inconclusive_is_zero():
    rep = EvalReport(
        results=[_case(0.5, inconclusive=True)],
        total_cases=1, passed_cases=0,
    )
    assert rep.avg_score == 0.0


def test_decide_rejects_below_min_cases():
    from echo_agent.evolution.gate import PromotionGate
    from echo_agent.evaluation.runner import EvalReport
    gate = PromotionGate.__new__(PromotionGate)
    gate._regression_threshold = 0.05
    gate._require_strict = False
    gate._min_eval_cases = 3
    baseline = EvalReport(total_cases=1, passed_cases=0)
    cand = EvalReport(total_cases=1, passed_cases=1)  # 单 case 翻转
    decision = gate._decide(baseline, cand)
    assert decision.promoted is False
    assert "inconclusive" in decision.reason


def test_decide_allows_at_or_above_min_cases():
    from echo_agent.evolution.gate import PromotionGate
    from echo_agent.evaluation.runner import EvalReport
    gate = PromotionGate.__new__(PromotionGate)
    gate._regression_threshold = 0.05
    gate._require_strict = False
    gate._min_eval_cases = 3
    baseline = EvalReport(total_cases=3, passed_cases=1)
    cand = EvalReport(total_cases=3, passed_cases=3)
    decision = gate._decide(baseline, cand)
    assert decision.promoted is True
