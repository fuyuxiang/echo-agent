"""Data structures for worker delegation."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class WorkerProfile:
    """Template for a worker agent — provides default instructions and tools."""

    id: str
    name: str
    description: str = ""
    instructions: str = ""
    default_tools: tuple[str, ...] = ()
    model: str = ""
    provider: str = ""
    max_iterations: int = 12
    max_tokens: int = 4096
    temperature: float = 0.4


@dataclass
class WorkerResult:
    """Result from a single worker execution."""

    task_index: int
    status: str = "completed"  # completed | failed | timeout
    output: str = ""
    error: str = ""
    iterations: int = 0
    tool_calls: int = 0
    duration_seconds: float = 0.0
    model: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)
