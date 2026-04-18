"""Task planning, state machine, and sub-agent delegation.

Covers:
  - Goal understanding and subtask decomposition
  - Execution ordering and dependency resolution
  - Full task state machine (pending → running → success/failed/retry/cancelled/suspended)
  - Sub-agent spawning with independent context, tools, and parallel execution
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any, Awaitable, Callable

from loguru import logger


class TaskStatus(str, Enum):
    PENDING = "pending"
    QUEUED = "queued"
    RUNNING = "running"
    SUCCESS = "success"
    FAILED = "failed"
    RETRYING = "retrying"
    CANCELLED = "cancelled"
    SUSPENDED = "suspended"  # waiting for external input


@dataclass
class TaskRecord:
    id: str = field(default_factory=lambda: f"task_{uuid.uuid4().hex[:8]}")
    parent_id: str | None = None
    workflow_id: str = ""
    title: str = ""
    description: str = ""
    kind: str = "general"
    status: TaskStatus = TaskStatus.PENDING
    priority: int = 0
    dependencies: list[str] = field(default_factory=list)
    subtask_ids: list[str] = field(default_factory=list)
    result: str = ""
    error: str = ""
    attempt: int = 0
    max_retries: int = 2
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())
    updated_at: str = field(default_factory=lambda: datetime.now().isoformat())
    metadata: dict[str, Any] = field(default_factory=dict)

    def can_start(self, completed_ids: set[str]) -> bool:
        return all(dep in completed_ids for dep in self.dependencies)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "parent_id": self.parent_id, "workflow_id": self.workflow_id,
            "title": self.title, "description": self.description, "kind": self.kind,
            "status": self.status.value, "priority": self.priority,
            "dependencies": self.dependencies, "subtask_ids": self.subtask_ids,
            "result": self.result, "error": self.error, "attempt": self.attempt,
            "max_retries": self.max_retries, "created_at": self.created_at,
            "updated_at": self.updated_at, "metadata": self.metadata,
        }
