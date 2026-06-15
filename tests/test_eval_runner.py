"""Tests for EvalRunner semantic metric wiring."""
from __future__ import annotations

import pytest

from echo_agent.evaluation.runner import EvalRunner
from echo_agent.evaluation.dataset import EvalCase


class _FakeProvider:
    def __init__(self):
        self.calls = 0

    async def chat_with_retry(self, **kwargs):
        self.calls += 1
        class _Resp:
            content = '{"score": 0.8, "reasoning": "ok"}'
        return _Resp()


def _case(expected_output: str = "") -> EvalCase:
    return EvalCase(
        id="t", input="hi", expected_tools=[], expected_contains=[],
        expected_output=expected_output, max_iterations=3, tags=[], metadata={},
    )


@pytest.mark.asyncio
async def test_score_response_no_provider_skips_semantic():
    runner = EvalRunner(agent_loop=None, provider=None)
    metrics = await runner._score_response(_case("some expected text"), "actual", [], 1)
    names = [m.name for m in metrics]
    assert "quality" in names
    assert "semantic_quality" not in names


@pytest.mark.asyncio
async def test_score_response_with_provider_adds_semantic():
    provider = _FakeProvider()
    runner = EvalRunner(agent_loop=None, provider=provider)
    metrics = await runner._score_response(_case("some expected text"), "actual", [], 1)
    names = [m.name for m in metrics]
    assert "quality" in names
    assert "semantic_quality" in names
    assert provider.calls == 1


@pytest.mark.asyncio
async def test_score_response_no_expected_output_skips_both():
    provider = _FakeProvider()
    runner = EvalRunner(agent_loop=None, provider=provider)
    metrics = await runner._score_response(_case(""), "actual", [], 1)
    names = [m.name for m in metrics]
    assert "quality" not in names
    assert "semantic_quality" not in names
    assert provider.calls == 0
