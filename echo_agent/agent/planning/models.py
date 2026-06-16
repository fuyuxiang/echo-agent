from __future__ import annotations
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class StrategyType(str, Enum):
    REACT = "react"
    PLAN_EXECUTE = "plan_execute"
    TREE_OF_THOUGHT = "tree_of_thought"
    LATS = "lats"


class StepStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    SKIPPED = "skipped"


@dataclass
class PlanStep:
    index: int
    description: str
    tool_hint: str = ""  # suggested tool name
    status: StepStatus = StepStatus.PENDING
    result: str = ""
    reasoning: str = ""


@dataclass
class Plan:
    strategy: StrategyType
    steps: list[PlanStep] = field(default_factory=list)
    goal: str = ""
    current_step: int = 0
    is_complete: bool = False
    reflection: str = ""

    def next_step(self) -> PlanStep | None:
        for step in self.steps:
            if step.status == StepStatus.PENDING:
                return step
        return None

    def mark_step_complete(self, index: int, result: str) -> None:
        if 0 <= index < len(self.steps):
            self.steps[index].status = StepStatus.COMPLETED
            self.steps[index].result = result
            self.current_step = index + 1
            if all(s.status in (StepStatus.COMPLETED, StepStatus.SKIPPED) for s in self.steps):
                self.is_complete = True

    def mark_step_failed(self, index: int, error: str) -> None:
        if 0 <= index < len(self.steps):
            self.steps[index].status = StepStatus.FAILED
            self.steps[index].result = error

    def to_prompt(self) -> str:
        lines = [f"Plan ({self.strategy.value}): {self.goal}"]
        for s in self.steps:
            marker = "✓" if s.status == StepStatus.COMPLETED else "✗" if s.status == StepStatus.FAILED else "→" if s.status == StepStatus.RUNNING else "○"
            lines.append(f"  {marker} Step {s.index}: {s.description}")
            if s.result:
                lines.append(f"    Result: {s.result[:200]}")
        return "\n".join(lines)

    def to_dict(self) -> dict[str, Any]:
        return {
            "strategy": self.strategy.value,
            "goal": self.goal,
            "current_step": self.current_step,
            "is_complete": self.is_complete,
            "reflection": self.reflection,
            "steps": [
                {
                    "index": s.index,
                    "description": s.description,
                    "tool_hint": s.tool_hint,
                    "status": s.status.value,
                    "result": s.result,
                    "reasoning": s.reasoning,
                }
                for s in self.steps
            ],
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Plan":
        steps = [
            PlanStep(
                index=s.get("index", i),
                description=s.get("description", ""),
                tool_hint=s.get("tool_hint", ""),
                status=StepStatus(s.get("status", "pending")),
                result=s.get("result", ""),
                reasoning=s.get("reasoning", ""),
            )
            for i, s in enumerate(data.get("steps", []))
        ]
        return cls(
            strategy=StrategyType(data.get("strategy", "react")),
            steps=steps,
            goal=data.get("goal", ""),
            current_step=data.get("current_step", 0),
            is_complete=data.get("is_complete", False),
            reflection=data.get("reflection", ""),
        )


@dataclass
class StepAction:
    action: str  # "execute_tool", "respond", "replan", "backtrack"
    tool_name: str = ""
    tool_args: dict[str, Any] = field(default_factory=dict)
    reasoning: str = ""


@dataclass
class Feedback:
    should_replan: bool = False
    critique: str = ""
    suggestions: list[str] = field(default_factory=list)
    score: float = 0.0
