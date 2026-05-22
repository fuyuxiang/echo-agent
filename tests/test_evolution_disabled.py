"""Tests for evolution-disabled paths — AgentLoop must work without it."""

from __future__ import annotations

from pathlib import Path

import pytest

from echo_agent.config.loader import load_config


@pytest.mark.asyncio
async def test_agent_loop_starts_with_evolution_disabled(tmp_path: Path):
    """An AgentLoop must boot cleanly when evolution.enabled = False."""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.models.provider import LLMProvider, LLMResponse

    config = load_config(overrides={"workspace": str(tmp_path)})
    config.evolution.enabled = False
    config.knowledge.enabled = False  # avoid heavy index init

    class _Stub(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
            return LLMResponse(content="ok")
        def get_default_model(self):
            return "stub"

    loop = AgentLoop(
        bus=MessageBus(),
        config=config,
        provider=_Stub(),
        workspace=tmp_path,
    )
    assert loop.evolution is None
    await loop.start()
    try:
        # Tools registry should not include evolution tools.
        names = set(loop.tools.tool_names)
        assert "evolution_status" not in names
        assert "evolution_run" not in names
        assert "evolution_rollback" not in names
    finally:
        await loop.stop()


@pytest.mark.asyncio
async def test_agent_loop_attaches_evolution_when_engine_set(tmp_path: Path):
    """AgentLoop.set_evolution_engine registers the three evolution tools."""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.models.provider import LLMProvider, LLMResponse
    from unittest.mock import AsyncMock, MagicMock

    config = load_config(overrides={"workspace": str(tmp_path)})
    config.evolution.enabled = True
    config.knowledge.enabled = False

    class _Stub(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
            return LLMResponse(content="ok")
        def get_default_model(self):
            return "stub"

    loop = AgentLoop(
        bus=MessageBus(),
        config=config,
        provider=_Stub(),
        workspace=tmp_path,
    )

    fake_engine = MagicMock()
    fake_engine.start = AsyncMock()
    fake_engine.stop = AsyncMock()
    fake_engine.recorder = MagicMock()
    fake_engine.recorder.begin_turn = AsyncMock()
    fake_engine.recorder.end_turn = AsyncMock()

    loop.set_evolution_engine(fake_engine)

    names = set(loop.tools.tool_names)
    assert "evolution_status" in names
    assert "evolution_run" in names
    assert "evolution_rollback" in names

    await loop.start()
    try:
        fake_engine.start.assert_awaited_once()
    finally:
        await loop.stop()
        fake_engine.stop.assert_awaited_once()
