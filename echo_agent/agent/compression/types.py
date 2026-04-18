"""Shared data structures for the compression engine."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class CompressionStats:
    compression_count: int = 0
    last_compressed_at: float | None = None
    last_summary_failure_at: float | None = None
    total_tokens_saved: int = 0
    warnings: list[str] = field(default_factory=list)


@dataclass
class BoundaryResult:
    head_end: int
    tail_start: int
    head_messages: list[dict[str, Any]]
    middle_messages: list[dict[str, Any]]
    tail_messages: list[dict[str, Any]]
    no_compression_needed: bool = False


@dataclass
class PruneResult:
    messages: list[dict[str, Any]]
    pruned_count: int


@dataclass
class CompressionResult:
    messages: list[dict[str, Any]]
    stats: CompressionStats
    summary_text: str | None = None
    was_compressed: bool = False
    tokens_before: int = 0
    tokens_after: int = 0
