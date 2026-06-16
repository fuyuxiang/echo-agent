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


def test_not_contains_ignores_empty_string():
    r = not_contains([""], "anything at all")
    assert r.passed is True


def test_not_contains_dedups_violations():
    r = not_contains(["pw", "pw"], "my pw here")
    assert r.details["violations"] == ["pw"]


import pytest
from echo_agent.evaluation.semantic_metrics import semantic_quality


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
