"""Ebbinghaus adaptive forgetting — spaced repetition, decay scanning, archival."""

from __future__ import annotations

import math
from datetime import datetime
from typing import TYPE_CHECKING

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.memory.types import MemoryEntry


class ForgettingCurve:
    """Adaptive forgetting inspired by Ebbinghaus with spaced-repetition reinforcement.

    Core formula:
        half_life = base_half_life * (1 + log2(1 + access_count))
        decay = 0.5 ^ (days_since_access / half_life)
        effective_importance = importance * decay

    More accesses → longer half-life → slower forgetting.
    """

    def __init__(
        self,
        base_half_life_days: float = 30.0,
        archive_threshold: float = 0.05,
        forget_threshold: float = 0.01,
    ):
        self._base_half_life = max(1.0, base_half_life_days)
        self._archive_threshold = archive_threshold
        self._forget_threshold = forget_threshold

    @staticmethod
    def _days_since(iso_timestamp: str) -> float:
        """Days elapsed since *iso_timestamp*, tolerant of tz-aware values."""
        last = datetime.fromisoformat(iso_timestamp)
        now = datetime.now(last.tzinfo) if last.tzinfo else datetime.now()
        return (now - last).total_seconds() / 86400

    def effective_importance(self, entry: MemoryEntry) -> float:
        # Core philosophy: facts about the user (identity, preferences, family,
        # long-term goals) are the relationship core. They are durable by nature
        # and must never decay — a personal assistant forgetting your birthday
        # because you haven't mentioned it in a while is unacceptable. USER
        # memories are pinned to their raw importance, exempt from the curve.
        from echo_agent.memory.types import MemoryType
        if entry.type == MemoryType.USER:
            return entry.importance
        if not entry.last_accessed or self._base_half_life <= 0:
            return entry.importance
        try:
            days = self._days_since(entry.last_accessed)
            if days < 0:
                return entry.importance
            half_life = self._base_half_life * (1 + math.log2(1 + entry.access_count))
            decay = math.pow(0.5, days / half_life)
            return entry.importance * decay
        except (ValueError, OverflowError, TypeError):
            # TypeError included: a single tz-aware timestamp mixed with naive
            # ones must degrade gracefully, not blow up inside sort lambdas.
            return entry.importance

    def half_life_days(self, entry: MemoryEntry) -> float:
        return self._base_half_life * (1 + math.log2(1 + entry.access_count))

    def should_archive(self, entry: MemoryEntry) -> bool:
        eff = self.effective_importance(entry)
        return 0 < eff < self._archive_threshold

    def should_forget(self, entry: MemoryEntry) -> bool:
        eff = self.effective_importance(entry)
        return 0 < eff < self._forget_threshold

    def days_until_archive(self, entry: MemoryEntry) -> float | None:
        if entry.importance <= 0 or entry.importance <= self._archive_threshold:
            return 0.0
        if not entry.last_accessed:
            return None
        half_life = self.half_life_days(entry)
        target_ratio = self._archive_threshold / entry.importance
        if target_ratio >= 1.0:
            return 0.0
        days_needed = -half_life * math.log2(target_ratio)
        try:
            elapsed = self._days_since(entry.last_accessed)
            remaining = days_needed - elapsed
            return max(0.0, remaining)
        except (ValueError, OverflowError, TypeError):
            return None

    async def run_decay_pass(
        self,
        entries: list[MemoryEntry],
    ) -> tuple[list[MemoryEntry], list[MemoryEntry]]:
        """Scan entries and classify into archive/forget lists.

        Returns (to_archive, to_forget).
        """
        to_archive: list[MemoryEntry] = []
        to_forget: list[MemoryEntry] = []
        for entry in entries:
            from echo_agent.memory.types import MemoryTier, MemoryType
            # User facts are the relationship core — never archive or forget them,
            # regardless of importance score. Hard guarantee, not score-dependent.
            if entry.type == MemoryType.USER:
                continue
            if entry.tier == MemoryTier.ARCHIVAL:
                if self.should_forget(entry):
                    to_forget.append(entry)
                continue
            if entry.tier == MemoryTier.WORKING:
                continue
            if self.should_forget(entry):
                to_forget.append(entry)
            elif self.should_archive(entry):
                to_archive.append(entry)
        logger.info("Decay pass: {} to archive, {} to forget", len(to_archive), len(to_forget))
        return to_archive, to_forget
