"""Evaluation metrics — scoring functions for agent responses."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class MetricResult:
    name: str = ""
    score: float = 0.0
    passed: bool = False
    inconclusive: bool = False
    details: dict[str, Any] = field(default_factory=dict)


def exact_match(expected: str, actual: str) -> MetricResult:
    match = expected.strip().lower() == actual.strip().lower()
    return MetricResult(name="exact_match", score=1.0 if match else 0.0, passed=match)


def contains_all(expected_substrings: list[str], actual: str) -> MetricResult:
    if not expected_substrings:
        return MetricResult(name="contains_all", score=1.0, passed=True)
    lower = actual.lower()
    hits = sum(1 for s in expected_substrings if s.lower() in lower)
    score = hits / len(expected_substrings)
    return MetricResult(
        name="contains_all", score=score, passed=score == 1.0,
        details={"matched": hits, "total": len(expected_substrings)},
    )


def tool_usage_correctness(expected_tools: list[str], actual_tools: list[str]) -> MetricResult:
    if not expected_tools:
        return MetricResult(name="tool_usage", score=1.0, passed=True)
    expected_set = set(expected_tools)
    actual_set = set(actual_tools)
    if not expected_set:
        return MetricResult(name="tool_usage", score=1.0, passed=True)
    recall = len(expected_set & actual_set) / len(expected_set)
    precision = len(expected_set & actual_set) / len(actual_set) if actual_set else 0.0
    f1 = 2 * precision * recall / (precision + recall) if (precision + recall) > 0 else 0.0
    return MetricResult(
        name="tool_usage", score=f1, passed=recall == 1.0,
        details={"expected": list(expected_set), "actual": list(actual_set), "precision": precision, "recall": recall},
    )


def iteration_efficiency(actual_iterations: int, max_allowed: int) -> MetricResult:
    if max_allowed <= 0:
        return MetricResult(name="efficiency", score=1.0, passed=True)
    passed = actual_iterations <= max_allowed
    score = max(0.0, 1.0 - (actual_iterations - 1) / max(max_allowed, 1))
    return MetricResult(
        name="efficiency", score=score, passed=passed,
        details={"iterations": actual_iterations, "max": max_allowed},
    )


def response_quality(expected_output: str, actual: str) -> MetricResult:
    if not expected_output:
        return MetricResult(name="quality", score=1.0, passed=True)
    # Simple overlap scoring
    expected_words = set(expected_output.lower().split())
    actual_words = set(actual.lower().split())
    if not expected_words:
        return MetricResult(name="quality", score=1.0, passed=True)
    overlap = len(expected_words & actual_words) / len(expected_words)
    return MetricResult(name="quality", score=overlap, passed=overlap > 0.5)


def not_contains(forbidden_substrings: list[str], actual: str) -> MetricResult:
    """Fail if ANY forbidden substring appears in the response (case-insensitive)."""
    if not forbidden_substrings:
        return MetricResult(name="not_contains", score=1.0, passed=True)
    lower = actual.lower()
    seen = set()
    hits = []
    for s in forbidden_substrings:
        if s and s.lower() in lower and s not in seen:
            seen.add(s)
            hits.append(s)
    passed = not hits
    return MetricResult(
        name="not_contains", score=1.0 if passed else 0.0, passed=passed,
        details={"violations": hits},
    )


def forbidden_tools_check(forbidden_tools: list[str], actual_tools: list[str]) -> MetricResult:
    """Fail if ANY forbidden tool was used."""
    if not forbidden_tools:
        return MetricResult(name="forbidden_tools", score=1.0, passed=True)
    used = set(actual_tools)
    seen = set()
    hits = []
    for t in forbidden_tools:
        if t and t in used and t not in seen:
            seen.add(t)
            hits.append(t)
    passed = not hits
    return MetricResult(
        name="forbidden_tools", score=1.0 if passed else 0.0, passed=passed,
        details={"violations": hits},
    )
