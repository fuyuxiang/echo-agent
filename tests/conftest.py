"""Shared test fixtures and helpers for echo-agent test suite."""

import uuid
import json
from datetime import datetime
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest


# ── Common response helpers ──────────────────────────────────────────────────


class FakeLLMResponse:
    """Minimal LLM response stub for testing."""

    def __init__(self, content="", tool_calls=None):
        self.content = content
        self.tool_calls = tool_calls or []


class FakeToolCall:
    """Minimal tool call stub."""

    def __init__(self, name="", arguments=None):
        self.name = name
        self.arguments = arguments if arguments is not None else {}
        self.id = uuid.uuid4().hex[:8]


def make_llm_response(content="done", tool_calls=None):
    """Create a fake LLM response for use in mock side_effect lists."""
    return FakeLLMResponse(content=content, tool_calls=tool_calls)


def make_tool_call(name, **kwargs):
    """Create a fake tool call."""
    return FakeToolCall(name=name, arguments=json.dumps(kwargs))


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture
def mock_provider():
    """LLM provider mock with configurable responses."""
    provider = AsyncMock()
    provider.chat_with_retry = AsyncMock(return_value=FakeLLMResponse())
    provider.embed = AsyncMock(return_value=[0.1] * 1536)
    return provider


@pytest.fixture
def mock_llm_call():
    """Standalone async LLM call mock."""
    return AsyncMock(return_value=FakeLLMResponse())


@pytest.fixture
def tmp_workspace(tmp_path):
    """Temporary workspace directory with skills subdirectory."""
    skills_dir = tmp_path / "skills"
    skills_dir.mkdir()
    return tmp_path


# ── Trajectory helpers ───────────────────────────────────────────────────────


def make_trajectory(
    *,
    outcome="success",
    tool_name="test_tool",
    reflection_score=0.8,
    session_key="test-session",
    trajectory_id=None,
):
    """Create a trajectory dict for evolution engine tests."""
    return {
        "id": trajectory_id or uuid.uuid4().hex[:12],
        "session_key": session_key,
        "tool_name": tool_name,
        "outcome": outcome,
        "reflection_score": reflection_score,
        "created_at": datetime.now().isoformat(),
        "consumed_by_run": None,
    }


# ── Evolution helpers ────────────────────────────────────────────────────────


def make_propose_call(skill_name="new-skill", operation="create", content="# SKILL"):
    """Create a tool call for skill proposal in evolution tests."""
    args = {
        "skill_name": skill_name,
        "operation": operation,
        "proposed_content": content,
    }
    return FakeToolCall(name="propose_skill", arguments=json.dumps(args))
