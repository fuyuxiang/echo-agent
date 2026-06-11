"""Agent-facing tools for the self-evolution subsystem.

These are intentionally small wrappers over :class:`EvolutionEngine` — they
let the agent itself inspect or trigger evolution, but every state change
goes through the same :class:`PromotionGate` as the CLI.
"""

from __future__ import annotations

import json
from typing import Any

from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.evolution.engine import EvolutionEngine


class EvolutionStatusTool(Tool):
    name = "evolution_status"
    description = (
        "Show the status of the self-evolving skill harness — most recent run, "
        "pending and promoted candidate counts, cooldowns, and unconsumed "
        "trajectory count."
    )
    parameters: dict[str, Any] = {"type": "object", "properties": {}, "required": []}
    risk_level = "read_only"

    def __init__(self, engine: EvolutionEngine):
        self._engine = engine

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            summary = await self._engine.status_summary()
            return ToolResult(success=True, output=json.dumps(summary, ensure_ascii=False, indent=2))
        except Exception as e:
            return ToolResult(success=False, error=f"evolution_status failed: {e}")


class EvolutionRunTool(Tool):
    name = "evolution_run"
    description = (
        "Trigger a self-evolution pass right now. Generates skill candidates from "
        "recent trajectories and runs an A/B evaluation against the baseline "
        "dataset before promoting any change. High-risk: requires approval."
    )
    parameters: dict[str, Any] = {"type": "object", "properties": {}, "required": []}
    risk_level = "dangerous"

    def __init__(self, engine: EvolutionEngine):
        self._engine = engine

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "side_effect"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            run = await self._engine.run_evolution(trigger="manual")
            return ToolResult(
                success=True,
                output=json.dumps(run.to_dict(), ensure_ascii=False, indent=2),
            )
        except Exception as e:
            return ToolResult(success=False, error=f"evolution_run failed: {e}")


class EvolutionRollbackTool(Tool):
    name = "evolution_rollback"
    description = (
        "Roll back the most recent promoted change for a given skill. "
        "Use when a promoted skill turns out to be harmful. High-risk: "
        "requires approval."
    )
    parameters: dict[str, Any] = {
        "type": "object",
        "properties": {
            "skill_name": {
                "type": "string",
                "description": "Name of the skill to roll back.",
            },
        },
        "required": ["skill_name"],
    }
    risk_level = "dangerous"

    def __init__(self, engine: EvolutionEngine):
        self._engine = engine

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "side_effect"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        skill_name = (params.get("skill_name") or "").strip()
        if not skill_name:
            return ToolResult(success=False, error="skill_name is required")
        try:
            ok, message = await self._engine.rollback_skill(skill_name)
            return ToolResult(success=ok, output=message if ok else "", error="" if ok else message)
        except Exception as e:
            return ToolResult(success=False, error=f"evolution_rollback failed: {e}")


def build_evolution_tools(engine: EvolutionEngine) -> list[Tool]:
    """Construct the set of evolution tools to register on the ToolRegistry."""
    return [
        EvolutionStatusTool(engine),
        EvolutionRunTool(engine),
        EvolutionRollbackTool(engine),
    ]
