"""Tests for SkillReviewer — background skill creation and update logic."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.models.provider import LLMResponse, ToolCallRequest
from echo_agent.skills.reviewer import SkillReviewer, _MAX_REVIEW_ITERATIONS


def _make_store():
    store = MagicMock()
    store.create_skill = MagicMock(return_value=None)
    store.update_skill = MagicMock(return_value=None)
    store.patch_skill = MagicMock(return_value=None)
    store.delete_skill = MagicMock(return_value=None)
    store.write_file = MagicMock(return_value=None)
    store.remove_file = MagicMock(return_value=None)
    return store


def _make_provider(responses):
    provider = AsyncMock()
    provider.chat_with_retry = AsyncMock(side_effect=responses)
    return provider


class TestSkillReviewerNoToolCalls:
    """review with no tool calls returns empty list."""

    @pytest.mark.asyncio
    async def test_no_tool_calls_empty_result(self):
        provider = _make_provider([
            LLMResponse(content="No skill changes needed.", finish_reason="stop"),
        ])
        store = _make_store()
        reviewer = SkillReviewer(provider=provider, store=store)

        conversation = [{"role": "user", "content": "Fix the typo in utils.py"}]
        actions = await reviewer.review(conversation)

        assert actions == []
        store.create_skill.assert_not_called()


class TestSkillReviewerCreateSkill:
    """review with skill_manage create tool call."""

    @pytest.mark.asyncio
    async def test_create_skill(self):
        tc = ToolCallRequest(
            id="call_1",
            name="skill_manage",
            arguments={"action": "create", "name": "deploy-steps", "content": "# Deploy\nSteps here"},
        )
        provider = _make_provider([
            LLMResponse(content="I'll create a skill.", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="Done.", finish_reason="stop"),
        ])
        store = _make_store()
        reviewer = SkillReviewer(provider=provider, store=store)

        conversation = [{"role": "user", "content": "Deploy to production"}]
        actions = await reviewer.review(conversation)

        assert len(actions) == 1
        assert "deploy-steps" in actions[0]
        store.create_skill.assert_called_once_with("deploy-steps", "# Deploy\nSteps here", category="")


class TestSkillReviewerLLMException:
    """LLM exception during review returns partial results."""

    @pytest.mark.asyncio
    async def test_llm_exception_partial_results(self):
        tc = ToolCallRequest(
            id="call_1",
            name="skill_manage",
            arguments={"action": "create", "name": "skill-a", "content": "# A"},
        )
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="Creating.", tool_calls=[tc], finish_reason="tool_calls"),
            Exception("API timeout"),
        ])
        store = _make_store()
        reviewer = SkillReviewer(provider=provider, store=store)

        conversation = [{"role": "user", "content": "Do something complex"}]
        actions = await reviewer.review(conversation)

        # First tool call succeeded, second LLM call raised
        assert len(actions) == 1
        assert "skill-a" in actions[0]

    @pytest.mark.asyncio
    async def test_llm_exception_no_results(self):
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=Exception("Connection refused"))
        store = _make_store()
        reviewer = SkillReviewer(provider=provider, store=store)

        conversation = [{"role": "user", "content": "hello"}]
        actions = await reviewer.review(conversation)

        assert actions == []


class TestSkillReviewerMaxIterations:
    """Reviewer respects max iterations limit."""

    @pytest.mark.asyncio
    async def test_max_iterations_limit(self):
        tc = ToolCallRequest(
            id="call_1",
            name="skill_manage",
            arguments={"action": "create", "name": "skill-loop", "content": "# Loop"},
        )
        # Provider always returns tool calls, never stops
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(
            return_value=LLMResponse(content="more", tool_calls=[tc], finish_reason="tool_calls")
        )
        store = _make_store()
        reviewer = SkillReviewer(provider=provider, store=store)

        conversation = [{"role": "user", "content": "loop test"}]
        actions = await reviewer.review(conversation)

        # Should be capped at _MAX_REVIEW_ITERATIONS
        assert len(actions) <= _MAX_REVIEW_ITERATIONS
        assert provider.chat_with_retry.call_count == _MAX_REVIEW_ITERATIONS
