"""Unified recall-eligibility policy shared by all retrieval/injection paths.

Lifecycle status is DERIVED, never stored — three states each have an
authoritative source (superseded_by field / tier==ARCHIVAL / in-memory
unresolved refcount). Adding a stored status field guarantees drift.
"""
from __future__ import annotations
from enum import Enum
from typing import Callable
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


def is_eligible(entry, audience: Audience, *, is_unresolved_fn: Callable[[str], bool]) -> bool:
    status = lifecycle_status(entry, is_unresolved_fn)
    return audience in _MATRIX[status]
