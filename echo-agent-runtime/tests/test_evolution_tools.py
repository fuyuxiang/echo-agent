"""Tests for echo_agent.evolution.tools — agent-facing tools."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.evolution.tools import (
    EvolutionRollbackTool,
    EvolutionRunTool,
    EvolutionStatusTool,
    build_evolution_tools,
)


def _engine_with(**methods) -> MagicMock:
    engine = MagicMock()
    for name, value in methods.items():
        setattr(engine, name, value)
    return engine


# ── Schema / metadata ────────────────────────────────────────────────────────


def test_status_tool_metadata():
    tool = EvolutionStatusTool(_engine_with())
    assert tool.name == "evolution_status"
    assert tool.risk_level == "read_only"
    assert tool.execution_mode({}) == "read_only"
    schema = tool.to_schema()
    assert schema["function"]["name"] == "evolution_status"


def test_run_tool_metadata():
    tool = EvolutionRunTool(_engine_with())
    assert tool.name == "evolution_run"
    assert tool.risk_level == "dangerous"
    assert tool.execution_mode({}) == "side_effect"


def test_rollback_tool_metadata():
    tool = EvolutionRollbackTool(_engine_with())
    assert tool.name == "evolution_rollback"
    assert tool.risk_level == "dangerous"
    assert tool.execution_mode({}) == "side_effect"
    # parameters schema declares skill_name as required
    assert "skill_name" in tool.parameters["properties"]
    assert "skill_name" in tool.parameters["required"]


# ── EvolutionStatusTool ───────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_status_tool_success_returns_json_payload():
    engine = _engine_with(status_summary=AsyncMock(return_value={
        "enabled": True,
        "candidates_pending": 2,
        "trajectories_unconsumed": 5,
    }))
    tool = EvolutionStatusTool(engine)

    result = await tool.execute({})
    assert result.success is True
    payload = json.loads(result.output)
    assert payload["enabled"] is True
    assert payload["candidates_pending"] == 2
    engine.status_summary.assert_awaited_once()


@pytest.mark.asyncio
async def test_status_tool_handles_engine_exception():
    engine = _engine_with(status_summary=AsyncMock(side_effect=RuntimeError("db down")))
    tool = EvolutionStatusTool(engine)

    result = await tool.execute({})
    assert result.success is False
    assert "evolution_status failed" in result.error
    assert "db down" in result.error


@pytest.mark.asyncio
async def test_status_tool_serializes_unicode_correctly():
    engine = _engine_with(status_summary=AsyncMock(return_value={"note": "中文 ✓"}))
    tool = EvolutionStatusTool(engine)
    result = await tool.execute({})
    assert result.success is True
    # ensure_ascii=False means we should see the literal characters.
    assert "中文" in result.output


# ── EvolutionRunTool ─────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_run_tool_success_returns_run_payload():
    fake_run = MagicMock()
    fake_run.to_dict.return_value = {
        "id": "run_abc",
        "triggered_by": "manual",
        "candidates_promoted": 1,
    }
    engine = _engine_with(run_evolution=AsyncMock(return_value=fake_run))
    tool = EvolutionRunTool(engine)

    result = await tool.execute({})
    assert result.success is True
    payload = json.loads(result.output)
    assert payload["id"] == "run_abc"
    assert payload["candidates_promoted"] == 1
    engine.run_evolution.assert_awaited_once_with(trigger="manual")


@pytest.mark.asyncio
async def test_run_tool_handles_engine_exception():
    engine = _engine_with(run_evolution=AsyncMock(side_effect=RuntimeError("eval crashed")))
    tool = EvolutionRunTool(engine)

    result = await tool.execute({})
    assert result.success is False
    assert "evolution_run failed" in result.error
    assert "eval crashed" in result.error


# ── EvolutionRollbackTool ────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_rollback_tool_success():
    engine = _engine_with(rollback_skill=AsyncMock(return_value=(True, "skill 'x' rolled back")))
    tool = EvolutionRollbackTool(engine)

    result = await tool.execute({"skill_name": "x"})
    assert result.success is True
    assert "rolled back" in result.output
    engine.rollback_skill.assert_awaited_once_with("x")


@pytest.mark.asyncio
async def test_rollback_tool_failure_returned_via_error_channel():
    engine = _engine_with(rollback_skill=AsyncMock(return_value=(False, "no promoted candidate")))
    tool = EvolutionRollbackTool(engine)

    result = await tool.execute({"skill_name": "missing"})
    assert result.success is False
    assert result.error == "no promoted candidate"
    assert result.output == ""


@pytest.mark.asyncio
async def test_rollback_tool_rejects_empty_skill_name():
    engine = _engine_with(rollback_skill=AsyncMock())
    tool = EvolutionRollbackTool(engine)

    result = await tool.execute({"skill_name": ""})
    assert result.success is False
    assert "skill_name is required" in result.error
    engine.rollback_skill.assert_not_called()


@pytest.mark.asyncio
async def test_rollback_tool_rejects_whitespace_only_skill_name():
    engine = _engine_with(rollback_skill=AsyncMock())
    tool = EvolutionRollbackTool(engine)

    result = await tool.execute({"skill_name": "   "})
    assert result.success is False
    engine.rollback_skill.assert_not_called()


@pytest.mark.asyncio
async def test_rollback_tool_handles_engine_exception():
    engine = _engine_with(rollback_skill=AsyncMock(side_effect=RuntimeError("FS error")))
    tool = EvolutionRollbackTool(engine)

    result = await tool.execute({"skill_name": "x"})
    assert result.success is False
    assert "evolution_rollback failed" in result.error
    assert "FS error" in result.error


@pytest.mark.asyncio
async def test_rollback_tool_strips_whitespace_around_name():
    engine = _engine_with(rollback_skill=AsyncMock(return_value=(True, "ok")))
    tool = EvolutionRollbackTool(engine)

    await tool.execute({"skill_name": "  alpha  "})
    engine.rollback_skill.assert_awaited_once_with("alpha")


# ── Factory ──────────────────────────────────────────────────────────────────


def test_build_evolution_tools_returns_three_tools():
    engine = _engine_with()
    tools = build_evolution_tools(engine)

    assert len(tools) == 3
    names = [t.name for t in tools]
    assert names == ["evolution_status", "evolution_run", "evolution_rollback"]
    # All tools share the same engine reference.
    for tool in tools:
        assert tool._engine is engine


def test_build_evolution_tools_distinct_instances_each_call():
    engine = _engine_with()
    a = build_evolution_tools(engine)
    b = build_evolution_tools(engine)
    # Each call constructs fresh objects so registries do not share lifecycle.
    assert a[0] is not b[0]
