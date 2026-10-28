"""Unified recall-eligibility policy shared by all retrieval/injection paths.

Lifecycle status is DERIVED, never stored — three states each have an
authoritative source (superseded_by field / tier==ARCHIVAL / in-memory
unresolved refcount). Adding a stored status field guarantees drift.
"""
from __future__ import annotations

import re
from enum import Enum
from typing import Any, Callable, Mapping

from echo_agent.memory.types import MemoryTier


class Audience(str, Enum):
    SNAPSHOT = "snapshot"
    RETRIEVAL = "retrieval"
    TOOL = "tool"
    ADMIN = "admin"
    MAINTENANCE = "maintenance"


class LifecycleStatus(str, Enum):
    ACTIVE = "active"
    UNRESOLVED = "unresolved"
    SUPERSEDED = "superseded"
    ARCHIVED = "archived"


def lifecycle_status(entry, is_unresolved_fn: Callable[[str], bool]) -> LifecycleStatus:
    # 固定判定顺序；对 _EpisodicProxy 用 getattr 兜底（无 tier/superseded_by）。
    if getattr(entry, "is_superseded", False):
        return LifecycleStatus.SUPERSEDED
    if getattr(entry, "tier", None) == MemoryTier.ARCHIVAL:
        return LifecycleStatus.ARCHIVED
    if is_unresolved_fn(getattr(entry, "id", "")):
        return LifecycleStatus.UNRESOLVED
    return LifecycleStatus.ACTIVE


# 资格矩阵（唯一权威）：status -> 允许可见的 audience 集合
_MATRIX: dict[LifecycleStatus, frozenset[Audience]] = {
    LifecycleStatus.ACTIVE: frozenset(Audience),
    LifecycleStatus.UNRESOLVED: frozenset({Audience.ADMIN, Audience.MAINTENANCE}),
    LifecycleStatus.SUPERSEDED: frozenset({Audience.ADMIN}),
    LifecycleStatus.ARCHIVED: frozenset({Audience.ADMIN, Audience.MAINTENANCE}),
}


# Task execution state belongs to the turn/session ledger, not semantic memory.
# Keep the classifier intentionally conservative: it only applies to facts that
# the model inferred or sleep consolidation distilled.  An explicit user-stated
# memory is never hidden, even if its key happens to contain ``status``.
_INFERRED_SOURCES = frozenset({"consolidated", "model_inferred"})
_TRANSIENT_TAGS = frozenset({
    "active_task",
    "checklist",
    "current_task",
    "task_progress",
    "task_state",
    "todo",
    "transient_task",
})
_DIRECT_TRANSIENT_KEYS = frozenset({
    "active_task",
    "checklist",
    "current_checklist",
    "current_task",
    "execution_progress",
    "execution_status",
    "next_steps",
    "pending_tasks",
    "task_progress",
    "task_state",
    "todo",
    "todos",
    "work_queue",
})
_STATE_KEY_PARTS = frozenset({"checklist", "phase", "progress", "status", "todo", "todos"})
_DIRECT_TASK_STATE_RE = re.compile(
    r"(?:当前任务|本次任务|任务进度|执行进度|执行状态|待办清单|"
    r"剩余(?:任务|事项)|下一步(?:是|为|\s*[:：])|"
    r"\b(?:current|active)\s+task\b|\btask\s+(?:progress|status)\b|"
    r"\bexecution\s+(?:progress|status)\b|\bremaining\s+(?:tasks?|items?)\b|"
    r"\bnext\s+steps?\b)",
    re.IGNORECASE,
)
_STATE_VALUE_RE = re.compile(
    r"(?:已完成|已处理|已拆分|已创建|完成了|待处理|待办|进行中|正在执行|"
    r"尚未完成|未完成|下一步|剩余|"
    r"\b(?:completed|finished|done|pending|in[ -]progress|remaining|next\s+steps?)\b)",
    re.IGNORECASE,
)


def _entry_field(entry: Any, name: str, default: Any = "") -> Any:
    if isinstance(entry, Mapping):
        return entry.get(name, default)
    return getattr(entry, name, default)


def is_transient_task_state(entry: Any, *, assumed_source: str = "") -> bool:
    """Return whether an inferred semantic fact is turn-local task state.

    This is a recall/write policy, not a deletion policy.  Existing entries
    remain available to ADMIN/MAINTENANCE audiences for inspection and repair.
    ``assumed_source`` lets consolidation classify a fact before provenance is
    stamped by ``promote_from_episodic``.
    """
    source = assumed_source or str(_entry_field(entry, "source", ""))
    if source not in _INFERRED_SOURCES:
        return False

    raw_tags = _entry_field(entry, "tags", []) or []
    if isinstance(raw_tags, str):
        raw_tags = [raw_tags]
    tags = {str(tag).strip().lower().replace("-", "_") for tag in raw_tags}
    if tags & _TRANSIENT_TAGS:
        return True

    key = str(_entry_field(entry, "key", "") or "").strip().lower()
    normalized_key = re.sub(r"[^0-9a-z\u4e00-\u9fff]+", "_", key).strip("_")
    if normalized_key in _DIRECT_TRANSIENT_KEYS:
        return True

    content = str(_entry_field(entry, "content", "") or "")
    if _DIRECT_TASK_STATE_RE.search(content):
        return True

    key_parts = set(normalized_key.split("_"))
    return bool(key_parts & _STATE_KEY_PARTS and _STATE_VALUE_RE.search(content))


def is_eligible(entry, audience: Audience, *, is_unresolved_fn: Callable[[str], bool]) -> bool:
    if audience in {Audience.SNAPSHOT, Audience.RETRIEVAL, Audience.TOOL}:
        if is_transient_task_state(entry):
            return False
    status = lifecycle_status(entry, is_unresolved_fn)
    return audience in _MATRIX[status]
