"""Data types for the self-evolving skill harness."""

from __future__ import annotations

import hashlib
import json
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Literal


def _now_iso() -> str:
    return datetime.now().isoformat()


def _new_id(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex[:12]}"


def digest(value: Any, *, max_chars: int = 200) -> str:
    """Produce a redacted, length-bounded representation of an arbitrary value.

    Used to keep tool args/results out of the persistent store while preserving
    enough signal for later analysis. The first 200 chars of the str repr are
    kept verbatim and a SHA-256 prefix is appended so equality checks survive.
    """
    if isinstance(value, (dict, list)):
        try:
            text = json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)
        except Exception:
            text = str(value)
    else:
        text = str(value)
    head = text[:max_chars]
    digest_hex = hashlib.sha256(text.encode("utf-8", errors="replace")).hexdigest()[:12]
    truncated = "…" if len(text) > max_chars else ""
    return f"{head}{truncated} [{digest_hex}]"


Outcome = Literal["success", "failure", "partial"]
CandidateOperation = Literal["create", "patch", "disable"]
CandidateStatus = Literal[
    "pending",
    "evaluating",
    "promoted",
    "rejected",
    "rolled_back",
    "needs_review",
]
EvolutionTrigger = Literal["manual", "threshold", "scheduled"]


@dataclass
class ToolCall:
    """One tool invocation in a trajectory (redacted by default)."""

    name: str = ""
    args_digest: str = ""
    result_digest: str = ""
    duration_ms: float = 0.0
    success: bool = True
    error: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "args_digest": self.args_digest,
            "result_digest": self.result_digest,
            "duration_ms": self.duration_ms,
            "success": self.success,
            "error": self.error,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ToolCall:
        return cls(
            name=data.get("name", ""),
            args_digest=data.get("args_digest", ""),
            result_digest=data.get("result_digest", ""),
            duration_ms=float(data.get("duration_ms", 0.0)),
            success=bool(data.get("success", True)),
            error=data.get("error", ""),
        )


@dataclass
class Trajectory:
    """A complete record of one user-task → response cycle."""

    id: str = field(default_factory=lambda: _new_id("traj"))
    session_id: str = ""
    chat_id: str = ""
    channel: str = ""
    task_input: str = ""
    task_type: str = ""
    tools_called: list[ToolCall] = field(default_factory=list)
    iterations: int = 0
    duration_ms: float = 0.0
    final_response: str = ""
    reflection_score: float | None = None
    reflection_critique: str = ""
    reflection_suggestions: list[str] = field(default_factory=list)
    failure_reason: str = ""
    outcome: Outcome = "success"
    skills_active: list[str] = field(default_factory=list)
    model_used: str = ""
    created_at: str = field(default_factory=_now_iso)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "session_id": self.session_id,
            "chat_id": self.chat_id,
            "channel": self.channel,
            "task_input": self.task_input,
            "task_type": self.task_type,
            "tools_called": [t.to_dict() for t in self.tools_called],
            "iterations": self.iterations,
            "duration_ms": self.duration_ms,
            "final_response": self.final_response,
            "reflection_score": self.reflection_score,
            "reflection_critique": self.reflection_critique,
            "reflection_suggestions": list(self.reflection_suggestions),
            "failure_reason": self.failure_reason,
            "outcome": self.outcome,
            "skills_active": list(self.skills_active),
            "model_used": self.model_used,
            "created_at": self.created_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Trajectory:
        return cls(
            id=data.get("id", _new_id("traj")),
            session_id=data.get("session_id", ""),
            chat_id=data.get("chat_id", ""),
            channel=data.get("channel", ""),
            task_input=data.get("task_input", ""),
            task_type=data.get("task_type", ""),
            tools_called=[ToolCall.from_dict(t) for t in data.get("tools_called", [])],
            iterations=int(data.get("iterations", 0)),
            duration_ms=float(data.get("duration_ms", 0.0)),
            final_response=data.get("final_response", ""),
            reflection_score=data.get("reflection_score"),
            reflection_critique=data.get("reflection_critique", ""),
            reflection_suggestions=list(data.get("reflection_suggestions", [])),
            failure_reason=data.get("failure_reason", ""),
            outcome=data.get("outcome", "success"),
            skills_active=list(data.get("skills_active", [])),
            model_used=data.get("model_used", ""),
            created_at=data.get("created_at", _now_iso()),
        )


@dataclass
class SkillCandidate:
    """A proposed skill change awaiting verification."""

    id: str = field(default_factory=lambda: _new_id("cand"))
    operation: CandidateOperation = "create"
    skill_name: str = ""
    parent_skill: str = ""
    proposed_content: str = ""
    proposed_patch_old: str = ""
    proposed_patch_new: str = ""
    rationale: str = ""
    expected_improvement: str = ""
    source_trajectories: list[str] = field(default_factory=list)
    status: CandidateStatus = "pending"
    eval_baseline: dict[str, Any] | None = None
    eval_with_candidate: dict[str, Any] | None = None
    promoted_at: str = ""
    rejected_reason: str = ""
    created_at: str = field(default_factory=_now_iso)
    run_id: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "operation": self.operation,
            "skill_name": self.skill_name,
            "parent_skill": self.parent_skill,
            "proposed_content": self.proposed_content,
            "proposed_patch_old": self.proposed_patch_old,
            "proposed_patch_new": self.proposed_patch_new,
            "rationale": self.rationale,
            "expected_improvement": self.expected_improvement,
            "source_trajectories": list(self.source_trajectories),
            "status": self.status,
            "eval_baseline": self.eval_baseline,
            "eval_with_candidate": self.eval_with_candidate,
            "promoted_at": self.promoted_at,
            "rejected_reason": self.rejected_reason,
            "created_at": self.created_at,
            "run_id": self.run_id,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SkillCandidate:
        return cls(
            id=data.get("id", _new_id("cand")),
            operation=data.get("operation", "create"),
            skill_name=data.get("skill_name", ""),
            parent_skill=data.get("parent_skill", ""),
            proposed_content=data.get("proposed_content", ""),
            proposed_patch_old=data.get("proposed_patch_old", ""),
            proposed_patch_new=data.get("proposed_patch_new", ""),
            rationale=data.get("rationale", ""),
            expected_improvement=data.get("expected_improvement", ""),
            source_trajectories=list(data.get("source_trajectories", [])),
            status=data.get("status", "pending"),
            eval_baseline=data.get("eval_baseline"),
            eval_with_candidate=data.get("eval_with_candidate"),
            promoted_at=data.get("promoted_at", ""),
            rejected_reason=data.get("rejected_reason", ""),
            created_at=data.get("created_at", _now_iso()),
            run_id=data.get("run_id", ""),
        )


@dataclass
class EvolutionRun:
    """Audit record of one full evolution pass."""

    id: str = field(default_factory=lambda: _new_id("run"))
    triggered_by: EvolutionTrigger = "manual"
    trajectories_consumed: int = 0
    candidates_generated: int = 0
    candidates_promoted: int = 0
    candidates_rejected: int = 0
    candidates_needs_review: int = 0
    duration_ms: float = 0.0
    started_at: str = field(default_factory=_now_iso)
    finished_at: str = ""
    error: str = ""
    notes: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "triggered_by": self.triggered_by,
            "trajectories_consumed": self.trajectories_consumed,
            "candidates_generated": self.candidates_generated,
            "candidates_promoted": self.candidates_promoted,
            "candidates_rejected": self.candidates_rejected,
            "candidates_needs_review": self.candidates_needs_review,
            "duration_ms": self.duration_ms,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "error": self.error,
            "notes": self.notes,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> EvolutionRun:
        return cls(
            id=data.get("id", _new_id("run")),
            triggered_by=data.get("triggered_by", "manual"),
            trajectories_consumed=int(data.get("trajectories_consumed", 0)),
            candidates_generated=int(data.get("candidates_generated", 0)),
            candidates_promoted=int(data.get("candidates_promoted", 0)),
            candidates_rejected=int(data.get("candidates_rejected", 0)),
            candidates_needs_review=int(data.get("candidates_needs_review", 0)),
            duration_ms=float(data.get("duration_ms", 0.0)),
            started_at=data.get("started_at", _now_iso()),
            finished_at=data.get("finished_at", ""),
            error=data.get("error", ""),
            notes=data.get("notes", ""),
        )
