"""Shared test fixtures and helpers for echo-agent test suite."""

import uuid
import json
from datetime import datetime
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio


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


@pytest_asyncio.fixture
async def gateway_ws_url():
    """Start an open-mode GatewayServer on loopback and yield its ws url."""
    from echo_agent.gateway.server import GatewayServer
    from echo_agent.config.schema import (
        GatewayConfig,
        GatewayAuthConfig,
        GatewaySessionPolicyConfig,
    )

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=0,
        auth=GatewayAuthConfig(mode="open", api_tokens=[]),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    from echo_agent.bus.queue import MessageBus

    bus = MessageBus()
    channel_manager = MagicMock()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))

    server = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=channel_manager,
        session_manager=session_manager,
        workspace=Path("/tmp/echo-agent-test-ws"),
        agent_loop=None,
    )
    await server.start()
    try:
        yield f"ws://127.0.0.1:{server.actual_port}/ws"
    finally:
        await server.stop()


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
