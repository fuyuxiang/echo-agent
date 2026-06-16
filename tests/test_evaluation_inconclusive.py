"""inconclusive 信号在 metric/result/report 间的传播。"""
from __future__ import annotations

from echo_agent.evaluation.metrics import (
    MetricResult,
    not_contains,
    forbidden_tools_check,
)


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
