"""Coordinator runtime — plans tasks, dispatches executors, handles fallback."""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.config.schema import AgentProfile, Config
from echo_agent.orchestrator.models import (
    AssignmentRecord,
    ExecutionLogRecord,
    RouteDecision,
    TaskRecord,
    WorkflowRecord,
)
from echo_agent.orchestrator.router import RoutePlanner, classify_provider_error
from echo_agent.orchestrator.store import WorkflowStore


@dataclass
class ExecutorResult:
    profile: AgentProfile
    task: TaskRecord
    content: str
    route: dict[str, Any] = field(default_factory=dict)
    tools_used: list[str] = field(default_factory=list)


class CoordinatorRuntime:
    """Plans tasks, dispatches executors, and persists workflow state."""

    def __init__(self, config: Config, store: WorkflowStore):
        self.config = config
        self.store = store
        self.router = RoutePlanner(config, store.load_health())

    async def run(
        self,
        *,
        session_key: str,
        channel: str,
        chat_id: str,
        content: str,
        media: list[str] | None = None,
        execute_task: Callable[..., Awaitable[ExecutorResult]],
        emit_update: Callable[[ExecutionLogRecord], Awaitable[None]] | None = None,
    ) -> tuple[WorkflowRecord, list[ExecutorResult], str]:
        workflow = WorkflowRecord(
            session_key=session_key, channel=channel, chat_id=chat_id,
            user_message=content,
            shared_board={"user_message": content, "media": media or []},
        )
        task_type = self.router.classify(content, media)
        profiles = self.router.choose_executors(task_type, content)

        tasks: list[TaskRecord] = []
        assignments: list[AssignmentRecord] = []
        for profile in profiles:
            task = TaskRecord(
                workflow_id=workflow.id,
                title=f"{profile.name} {task_type}",
                kind=task_type,
            )
            assignment = AssignmentRecord(
                workflow_id=workflow.id, task_id=task.id,
                agent_id=profile.id, agent_name=profile.name, agent_role=profile.role,
            )
            workflow.task_ids.append(task.id)
            workflow.assignment_ids.append(assignment.id)
            tasks.append(task)
            assignments.append(assignment)

        self.store.save_workflow(workflow, tasks, assignments)

        async def _run_one(profile: AgentProfile, task: TaskRecord, assignment: AssignmentRecord) -> ExecutorResult:
            self.router.replace_health(self.store.load_health())
            decision = self.router.choose_model(profile, task.kind, content)
            task.route = decision.to_dict()
            task.status = "running"
            assignment.status = "running"
            self.router.begin_load(profile.id)
            self.store.save_workflow(workflow, tasks, assignments)

            current = decision
            while True:
                try:
                    progress_cb = None
                    if emit_update and self.config.orchestration.show_executor_progress:
                        async def _emit(msg: str, tool_hint: bool = False, _p=profile, _t=task) -> None:
                            record = ExecutionLogRecord(
                                workflow_id=workflow.id, task_id=_t.id,
                                agent_id=_p.id, agent_name=_p.name,
                                message_kind="tool" if tool_hint else "progress",
                                content=msg, is_final=False,
                            )
                            self.store.append_log(workflow, tasks, assignments, record)
                            await emit_update(record)
                        progress_cb = _emit

                    result = await execute_task(
                        profile, task, current, session_key, channel, chat_id, content, media,
                    )
                    task.status = "success"
                    task.result = result.content
                    assignment.status = "done"
                    self.router.mark_success(current.provider)
                    self.router.end_load(profile.id)
                    self.store.save_workflow(workflow, tasks, assignments)
                    self.store.save_health(self.router.export_health())
                    return result

                except Exception as e:
                    logger.warning("Executor {} failed: {}", profile.id, e)
                    self.router.mark_failure(current.provider, e)
                    fallback = self.router.next_fallback_route(current)
                    if fallback is None:
                        task.status = "failed"
                        task.result = str(e)
                        assignment.status = "failed"
                        self.router.end_load(profile.id)
                        self.store.save_workflow(workflow, tasks, assignments)
                        self.store.save_health(self.router.export_health())
                        return ExecutorResult(profile=profile, task=task, content=f"Error: {e}")
                    current = fallback

        results = await asyncio.gather(
            *(_run_one(p, t, a) for p, t, a in zip(profiles, tasks, assignments)),
            return_exceptions=True,
        )

        executor_results: list[ExecutorResult] = []
        for r in results:
            if isinstance(r, Exception):
                logger.error("Executor exception: {}", r)
            elif isinstance(r, ExecutorResult):
                executor_results.append(r)

        successful = [r for r in executor_results if r.task.status == "success"]
        if successful:
            workflow.status = "success"
            final = "\n\n".join(r.content for r in successful if r.content)
        else:
            workflow.status = "failed"
            final = "All executors failed."

        self.store.save_workflow(workflow, tasks, assignments)
        return workflow, executor_results, final
