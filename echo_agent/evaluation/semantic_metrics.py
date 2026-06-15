"""Semantic evaluation metrics — LLM-as-judge scoring (I/O-bound).

Kept separate from metrics.py (pure synchronous functions) so that
async/LLM dependencies do not leak into the pure-function module.
"""
from __future__ import annotations

import json
from typing import TYPE_CHECKING

from loguru import logger

from echo_agent.evaluation.metrics import MetricResult

if TYPE_CHECKING:
    from echo_agent.models.provider import LLMProvider

_PASS_THRESHOLD = 0.7
_NEUTRAL_SCORE = 0.5

_SYSTEM_PROMPT = (
    "You are a judge that evaluates the quality of an AI assistant's reply. "
    "Focus only on semantic equivalence to the reference answer, not on wording. "
    "Respond with a single JSON object: {\"score\": <float 0.0-1.0>, "
    "\"reasoning\": \"<one short sentence>\"}. "
    "1.0 = semantically equivalent; 0.5 = partially relevant; 0.0 = unrelated or wrong."
)


def _build_user_prompt(expected: str, actual: str) -> str:
    return (
        f"Reference answer:\n{expected}\n\n"
        f"Assistant reply:\n{actual}\n\n"
        "Return the JSON object now."
    )


def _parse_score(content: str) -> float:
    """Extract a float score from the model's JSON reply. Raises on failure."""
    data = json.loads(content)
    return float(data["score"])


async def semantic_quality(
    expected: str,
    actual: str,
    provider: "LLMProvider",
    *,
    model: str | None = None,
) -> MetricResult:
    """Score how well `actual` matches `expected` in meaning, via LLM-as-judge.

    Failures (bad JSON, exceptions) return a neutral 0.5 so a flaky judge
    neither rewards nor penalizes a candidate.
    """
    messages = [
        {"role": "system", "content": _SYSTEM_PROMPT},
        {"role": "user", "content": _build_user_prompt(expected, actual)},
    ]
    try:
        resp = await provider.chat_with_retry(messages=messages, model=model)
        content = resp.content or ""
    except Exception as e:  # judge is best-effort; never break the eval run
        logger.warning("semantic_quality judge call failed: {}", e)
        return MetricResult(
            name="semantic_quality", score=_NEUTRAL_SCORE, passed=False,
            details={"error": str(e)},
        )

    try:
        raw_score = _parse_score(content)
    except (ValueError, KeyError, TypeError, json.JSONDecodeError):
        logger.warning("semantic_quality could not parse judge reply: {!r}", content[:200])
        return MetricResult(
            name="semantic_quality", score=_NEUTRAL_SCORE, passed=False,
            details={"raw": content[:500]},
        )

    score = max(0.0, min(1.0, raw_score))
    return MetricResult(
        name="semantic_quality", score=score, passed=score >= _PASS_THRESHOLD,
        details={"raw_score": raw_score},
    )
