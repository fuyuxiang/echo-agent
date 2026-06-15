"""Tests for LLM-as-judge semantic_quality metric."""
from __future__ import annotations

import pytest

from echo_agent.evaluation.semantic_metrics import semantic_quality


class _FakeProvider:
    """Minimal stub: returns a preset content or raises."""
    def __init__(self, content: str | None = None, exc: Exception | None = None):
        self._content = content
        self._exc = exc
        self.calls = 0

    async def chat_with_retry(self, **kwargs):
        self.calls += 1
        if self._exc is not None:
            raise self._exc
        class _Resp:
            content = self._content
        return _Resp()


@pytest.mark.asyncio
async def test_semantic_quality_high_score_passes():
    provider = _FakeProvider(content='{"score": 0.9, "reasoning": "equivalent"}')
    result = await semantic_quality("the capital is Paris", "Paris is the capital", provider)
    assert result.name == "semantic_quality"
    assert result.score == 0.9
    assert result.passed is True


@pytest.mark.asyncio
async def test_semantic_quality_low_score_fails():
    provider = _FakeProvider(content='{"score": 0.6, "reasoning": "partial"}')
    result = await semantic_quality("exp", "act", provider)
    assert result.score == 0.6
    assert result.passed is False


@pytest.mark.asyncio
async def test_semantic_quality_unparseable_is_neutral():
    provider = _FakeProvider(content="looks good to me")
    result = await semantic_quality("exp", "act", provider)
    assert result.score == 0.5
    assert result.passed is False
    assert "raw" in result.details


@pytest.mark.asyncio
async def test_semantic_quality_exception_is_neutral():
    provider = _FakeProvider(exc=RuntimeError("timeout"))
    result = await semantic_quality("exp", "act", provider)
    assert result.score == 0.5
    assert result.passed is False
    assert "error" in result.details


@pytest.mark.asyncio
async def test_semantic_quality_clamps_out_of_range():
    provider = _FakeProvider(content='{"score": 1.5, "reasoning": "x"}')
    result = await semantic_quality("exp", "act", provider)
    assert result.score == 1.0


@pytest.mark.asyncio
async def test_semantic_quality_nan_is_neutral():
    provider = _FakeProvider(content='{"score": NaN, "reasoning": "x"}')
    result = await semantic_quality("exp", "act", provider)
    assert result.score == 0.5
    assert result.passed is False


@pytest.mark.asyncio
async def test_semantic_quality_error_finish_reason_is_neutral():
    class _ErrResp:
        content = "Error: timed out"
        finish_reason = "error"
    class _ErrProvider:
        async def chat_with_retry(self, **kwargs):
            return _ErrResp()
    result = await semantic_quality("exp", "act", _ErrProvider())
    assert result.score == 0.5
    assert result.passed is False
    assert "error" in result.details


@pytest.mark.asyncio
async def test_semantic_quality_strips_markdown_fence():
    provider = _FakeProvider(content='```json\n{"score": 0.85, "reasoning": "ok"}\n```')
    result = await semantic_quality("exp", "act", provider)
    assert result.score == 0.85
    assert result.passed is True
