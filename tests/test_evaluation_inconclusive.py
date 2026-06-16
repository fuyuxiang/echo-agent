"""inconclusive 信号在 metric/result/report 间的传播。"""
from __future__ import annotations

from echo_agent.evaluation.metrics import MetricResult


def test_metric_result_inconclusive_defaults_false():
    m = MetricResult(name="x", score=1.0, passed=True)
    assert m.inconclusive is False


def test_metric_result_inconclusive_settable():
    m = MetricResult(name="x", score=0.5, passed=False, inconclusive=True)
    assert m.inconclusive is True
