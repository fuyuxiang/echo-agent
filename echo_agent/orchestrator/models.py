"""Orchestrator data models — workflow, task, route, health tracking."""

from __future__ import annotations

import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any


@dataclass
class ProviderHealthState:
    provider: str = ""
    status: str = "healthy"  # healthy | degraded | cooldown | disabled
    cooldown_until: str | None = None
    last_error_kind: str = ""
    failure_count: int = 0
    updated_at: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "provider": self.provider, "status": self.status,
            "cooldown_until": self.cooldown_until,
            "last_error_kind": self.last_error_kind,
            "failure_count": self.failure_count,
            "updated_at": self.updated_at,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> ProviderHealthState:
        return cls(**{k: d[k] for k in cls.__dataclass_fields__ if k in d})


@dataclass
class RouteDecision:
    trace_id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])
    parent_trace_id: str = ""
    provider: str = ""
    model: str = ""
    reason: str = ""
    route_kind: str = "primary"  # primary | fallback | override
    attempt_index: int = 0
    fallback_chain: list[str] = field(default_factory=list)
    health_score: float = 1.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "trace_id": self.trace_id, "parent_trace_id": self.parent_trace_id,
            "provider": self.provider, "model": self.model,
            "reason": self.reason, "route_kind": self.route_kind,
            "attempt_index": self.attempt_index,
            "fallback_chain": self.fallback_chain,
            "health_score": self.health_score,
        }

    def derive_child(self, provider: str, model: str, reason: str) -> RouteDecision:
        return RouteDecision(
            parent_trace_id=self.trace_id,
            provider=provider, model=model, reason=reason,
            route_kind="fallback",
            attempt_index=self.attempt_index + 1,
            fallback_chain=list(self.fallback_chain),
        )

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> RouteDecision:
        return cls(**{k: d[k] for k in cls.__dataclass_fields__ if k in d})


@dataclass
class TaskRecord:
    id: str = field(default_factory=lambda: f"task_{uuid.uuid4().hex[:8]}")
    workflow_id: str = ""
    title: str = ""
    kind: str = ""
    status: str = "queued"  # queued | running | success | failed | cancelled
    route: dict[str, Any] = field(default_factory=dict)
    result: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)
    created_at: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "workflow_id": self.workflow_id,
            "title": self.title, "kind": self.kind, "status": self.status,
            "route": self.route, "result": self.result,
            "metadata": self.metadata, "created_at": self.created_at,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> TaskRecord:
        return cls(**{k: d[k] for k in cls.__dataclass_fields__ if k in d})


@dataclass
class AssignmentRecord:
    id: str = field(default_factory=lambda: f"assign_{uuid.uuid4().hex[:8]}")
    workflow_id: str = ""
    task_id: str = ""
    agent_id: str = ""
    agent_name: str = ""
    agent_role: str = ""
    status: str = "pending"  # pending | running | done | failed

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "workflow_id": self.workflow_id,
            "task_id": self.task_id, "agent_id": self.agent_id,
            "agent_name": self.agent_name, "agent_role": self.agent_role,
            "status": self.status,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> AssignmentRecord:
        return cls(**{k: d[k] for k in cls.__dataclass_fields__ if k in d})


@dataclass
class ExecutionLogRecord:
    workflow_id: str = ""
    task_id: str = ""
    agent_id: str = ""
    agent_name: str = ""
    agent_role: str = ""
    message_kind: str = "final"
    content: str = ""
    is_final: bool = True
    timestamp: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "workflow_id": self.workflow_id, "task_id": self.task_id,
            "agent_id": self.agent_id, "agent_name": self.agent_name,
            "agent_role": self.agent_role, "message_kind": self.message_kind,
            "content": self.content, "is_final": self.is_final,
            "timestamp": self.timestamp,
        }


@dataclass
class WorkflowRecord:
    id: str = field(default_factory=lambda: f"wf_{uuid.uuid4().hex[:10]}")
    session_key: str = ""
    channel: str = ""
    chat_id: str = ""
    user_message: str = ""
    status: str = "running"  # running | success | failed
    task_ids: list[str] = field(default_factory=list)
    assignment_ids: list[str] = field(default_factory=list)
    shared_board: dict[str, Any] = field(default_factory=dict)
    execution_logs: list[dict[str, Any]] = field(default_factory=list)
    created_at: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "session_key": self.session_key,
            "channel": self.channel, "chat_id": self.chat_id,
            "user_message": self.user_message, "status": self.status,
            "task_ids": self.task_ids, "assignment_ids": self.assignment_ids,
            "shared_board": self.shared_board,
            "execution_logs": self.execution_logs,
            "created_at": self.created_at,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> WorkflowRecord:
        return cls(**{k: d[k] for k in cls.__dataclass_fields__ if k in d})
