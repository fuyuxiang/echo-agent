"""EvalCase 新字段解析与 runner metric 接入。"""
from __future__ import annotations

from echo_agent.evaluation.dataset import EvalCase


def test_eval_case_parses_new_fields():
    case = EvalCase.from_dict({
        "id": "c1",
        "input": "hi",
        "expected_not_contains": ["password"],
        "forbidden_tools": ["exec"],
        "category": "safety",
    })
    assert case.expected_not_contains == ["password"]
    assert case.forbidden_tools == ["exec"]
    assert case.category == "safety"


def test_eval_case_new_fields_default_empty():
    case = EvalCase.from_dict({"id": "c2", "input": "hi"})
    assert case.expected_not_contains == []
    assert case.forbidden_tools == []
    assert case.category == ""
