"""Task and workflow data models with state machine enforcement."""

from __future__ import annotations

import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any


# ── Task state machine ──────────────────────────────────────────────────────

class TaskStatus(str, Enum):
    PENDING = "pending"
    QUEUED = "queued"
    RUNNING = "running"
    SUCCESS = "success"
    FAILED = "failed"
    CANCELLED = "cancelled"
    SUSPENDED = "suspended"


VALID_TASK_TRANSITIONS: dict[TaskStatus, set[TaskStatus]] = {
    TaskStatus.PENDING: {TaskStatus.QUEUED, TaskStatus.RUNNING, TaskStatus.CANCELLED},
    TaskStatus.QUEUED: {TaskStatus.RUNNING, TaskStatus.CANCELLED},
    TaskStatus.RUNNING: {TaskStatus.SUCCESS, TaskStatus.FAILED, TaskStatus.CANCELLED, TaskStatus.SUSPENDED},
    TaskStatus.SUSPENDED: {TaskStatus.QUEUED, TaskStatus.RUNNING, TaskStatus.CANCELLED},
    TaskStatus.FAILED: {TaskStatus.QUEUED},  # retry
    TaskStatus.SUCCESS: set(),
    TaskStatus.CANCELLED: set(),
}

TERMINAL_TASK_STATUSES = {TaskStatus.SUCCESS, TaskStatus.FAILED, TaskStatus.CANCELLED}


def _gen_id(prefix: str = "t") -> str:
    return f"{prefix}_{uuid.uuid4().hex[:12]}"


def _now() -> str:
    return datetime.now().isoformat()

@dataclass
class TaskRecord:
    id: str = field(default_factory=lambda: _gen_id("t"))
    workflow_id: str = ""
    parent_task_id: str = ""
    title: str = ""
    description: str = ""
    status: TaskStatus = TaskStatus.PENDING
    priority: int = 5
    result: str = ""
    error: str = ""
    retry_count: int = 0
    max_retries: int = 3
    created_at: str = field(default_factory=_now)
    updated_at: str = field(default_factory=_now)
    started_at: str = ""
    completed_at: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "workflow_id": self.workflow_id,
            "parent_task_id": self.parent_task_id,
            "title": self.title, "description": self.description,
            "status": self.status.value, "priority": self.priority,
            "result": self.result, "error": self.error,
            "retry_count": self.retry_count, "max_retries": self.max_retries,
            "created_at": self.created_at, "updated_at": self.updated_at,
            "started_at": self.started_at, "completed_at": self.completed_at,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> TaskRecord:
        return cls(
            id=d["id"], workflow_id=d.get("workflow_id", ""),
            parent_task_id=d.get("parent_task_id", ""),
            title=d.get("title", ""), description=d.get("description", ""),
            status=TaskStatus(d.get("status", "pending")),
            priority=d.get("priority", 5),
            result=d.get("result", ""), error=d.get("error", ""),
            retry_count=d.get("retry_count", 0), max_retries=d.get("max_retries", 3),
            created_at=d.get("created_at", ""), updated_at=d.get("updated_at", ""),
            started_at=d.get("started_at", ""), completed_at=d.get("completed_at", ""),
            metadata=d.get("metadata", {}),
        )


# ── Workflow state machine ──────────────────────────────────────────────────

class WorkflowStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    WAITING = "waiting"
    BLOCKED = "blocked"
    SUCCESS = "success"
    FAILED = "failed"
    CANCELLED = "cancelled"


VALID_WORKFLOW_TRANSITIONS: dict[WorkflowStatus, set[WorkflowStatus]] = {
    WorkflowStatus.PENDING: {WorkflowStatus.RUNNING, WorkflowStatus.CANCELLED},
    WorkflowStatus.RUNNING: {WorkflowStatus.WAITING, WorkflowStatus.BLOCKED, WorkflowStatus.SUCCESS, WorkflowStatus.FAILED, WorkflowStatus.CANCELLED},
    WorkflowStatus.WAITING: {WorkflowStatus.RUNNING, WorkflowStatus.CANCELLED},
    WorkflowStatus.BLOCKED: {WorkflowStatus.RUNNING, WorkflowStatus.CANCELLED},
    WorkflowStatus.SUCCESS: set(),
    WorkflowStatus.FAILED: {WorkflowStatus.PENDING},  # retry whole workflow
    WorkflowStatus.CANCELLED: set(),
}

TERMINAL_WORKFLOW_STATUSES = {WorkflowStatus.SUCCESS, WorkflowStatus.FAILED, WorkflowStatus.CANCELLED}


@dataclass
class StepDefinition:
    id: str = ""
    name: str = ""
    tool_name: str = ""
    tool_params: dict[str, Any] = field(default_factory=dict)
    depends_on: list[str] = field(default_factory=list)
    condition: str = ""
    retry_max: int = 0
    timeout_seconds: int = 300

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "name": self.name,
            "tool_name": self.tool_name, "tool_params": self.tool_params,
            "depends_on": self.depends_on, "condition": self.condition,
            "retry_max": self.retry_max, "timeout_seconds": self.timeout_seconds,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> StepDefinition:
        return cls(
            id=d.get("id", ""), name=d.get("name", ""),
            tool_name=d.get("tool_name", ""), tool_params=d.get("tool_params", {}),
            depends_on=d.get("depends_on", []), condition=d.get("condition", ""),
            retry_max=d.get("retry_max", 0), timeout_seconds=d.get("timeout_seconds", 300),
        )


@dataclass
class WorkflowRecord:
    id: str = field(default_factory=lambda: _gen_id("wf"))
    name: str = ""
    description: str = ""
    status: WorkflowStatus = WorkflowStatus.PENDING
    steps: list[StepDefinition] = field(default_factory=list)
    step_tasks: dict[str, str] = field(default_factory=dict)  # step_id -> task_id
    current_step: str = ""
    state: dict[str, Any] = field(default_factory=dict)
    created_at: str = field(default_factory=_now)
    updated_at: str = field(default_factory=_now)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "name": self.name, "description": self.description,
            "status": self.status.value,
            "steps": [s.to_dict() for s in self.steps],
            "step_tasks": self.step_tasks,
            "current_step": self.current_step,
            "state": self.state,
            "created_at": self.created_at, "updated_at": self.updated_at,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> WorkflowRecord:
        return cls(
            id=d["id"], name=d.get("name", ""), description=d.get("description", ""),
            status=WorkflowStatus(d.get("status", "pending")),
            steps=[StepDefinition.from_dict(s) for s in d.get("steps", [])],
            step_tasks=d.get("step_tasks", {}),
            current_step=d.get("current_step", ""),
            state=d.get("state", {}),
            created_at=d.get("created_at", ""), updated_at=d.get("updated_at", ""),
        )
