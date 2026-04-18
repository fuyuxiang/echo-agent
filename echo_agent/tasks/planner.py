"""Task planner — decomposes goals into subtasks with dependency ordering."""

from __future__ import annotations

import json
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.tasks.models import TaskRecord, TaskStatus

_PLAN_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "create_plan",
            "description": "Create an execution plan with ordered subtasks.",
            "parameters": {
                "type": "object",
                "properties": {
                    "goal": {"type": "string", "description": "The understood goal."},
                    "subtasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "description": {"type": "string"},
                                "depends_on": {"type": "array", "items": {"type": "integer"}},
                            },
                            "required": ["title"],
                        },
                    },
                },
                "required": ["goal", "subtasks"],
            },
        },
    }
]


class TaskPlanner:
    """Uses LLM to decompose a goal into an ordered list of subtasks."""

    def __init__(self, llm_call: Callable[..., Awaitable[Any]]):
        self._llm_call = llm_call

    async def plan(self, goal: str, context: str = "") -> list[TaskRecord]:
        prompt = f"Break this goal into concrete subtasks with dependencies.\n\nGoal: {goal}"
        if context:
            prompt += f"\n\nContext:\n{context}"

        try:
            response = await self._llm_call(
                messages=[
                    {"role": "system", "content": "You are a task planning agent. Call create_plan with your plan."},
                    {"role": "user", "content": prompt},
                ],
                tools=_PLAN_TOOL,
                tool_choice={"type": "function", "function": {"name": "create_plan"}},
            )

            if not response.tool_calls:
                return [TaskRecord(title=goal, description="Single-step task")]

            args = response.tool_calls[0].arguments
            if isinstance(args, str):
                args = json.loads(args)

            subtasks_data = args.get("subtasks", [])
            tasks: list[TaskRecord] = []
            for i, st in enumerate(subtasks_data):
                task = TaskRecord(
                    title=st.get("title", f"Step {i + 1}"),
                    description=st.get("description", ""),
                    priority=i,
                )
                tasks.append(task)

            for i, st in enumerate(subtasks_data):
                deps = st.get("depends_on", [])
                for dep_idx in deps:
                    if 0 <= dep_idx < len(tasks):
                        tasks[i].dependencies.append(tasks[dep_idx].id)

            return tasks
        except Exception as e:
            logger.error("Planning failed: {}", e)
            return [TaskRecord(title=goal, description="Fallback single-step task")]

    async def replan(self, failed_task: TaskRecord, remaining: list[TaskRecord], error: str) -> list[TaskRecord]:
        context = f"Failed task: {failed_task.title}\nError: {error}\nRemaining: {[t.title for t in remaining]}"
        return await self.plan(f"Recover from failure and complete: {failed_task.title}", context)
