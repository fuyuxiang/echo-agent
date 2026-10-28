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
        lineage_max_versions: int = 3,
        lineage_retention_days: int = 90,
    ):
        self._base_half_life = max(1.0, base_half_life_days)
        self._archive_threshold = archive_threshold
        self._forget_threshold = forget_threshold
        self._lineage_max_versions = max(0, lineage_max_versions)
        self._lineage_retention_days = max(0, lineage_retention_days)

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

    def prune_lineage(self, entries: list[MemoryEntry]) -> list[MemoryEntry]:
        """世系裁剪：把过度膨胀的 superseded 版本链收敛到 ARCHIVAL。

        对每个 key 下的 **superseded** 版本（按 updated_at 倒序）：
          - 超过 lineage_max_versions 的更旧版本 → ARCHIVAL；
          - 超过 lineage_retention_days 天的陈旧版本 → ARCHIVAL（即使未超版本数）。
        **只作用于 superseded，绝不触碰 active 版本**（active 是唯一事实源）。
        返回本次被新标记为 ARCHIVAL 的条目列表。同步纯方法，供 store append 路径与
        run_decay_pass 复用。
        """
        from echo_agent.memory.types import MemoryTier

        marked: list[MemoryEntry] = []
        # 按 (key, source_session) 分组:多主体同 key(如两 session 都有 home)各自独立
        # 世系,共用同一 key 分组会把它们混入同一 lineage_max_versions 上限、越限误归档
        # 另一主体的版本。与写路径 _same_scope 的 source_session 隔离口径一致。
        by_key: dict[tuple[str, str], list[MemoryEntry]] = {}
        for entry in entries:
            if not entry.is_superseded:
                continue
            by_key.setdefault((entry.key, entry.source_session or ""), []).append(entry)

        for versions in by_key.values():
            # 倒序：最近更新的排前，保留最近 lineage_max_versions 版。
            versions.sort(key=lambda e: e.updated_at or "", reverse=True)
            for idx, entry in enumerate(versions):
                if entry.tier == MemoryTier.ARCHIVAL:
                    continue
                over_version = idx >= self._lineage_max_versions
                over_age = False
                if self._lineage_retention_days > 0 and entry.updated_at:
                    try:
                        over_age = self._days_since(entry.updated_at) > self._lineage_retention_days
                    except (ValueError, OverflowError, TypeError):
                        over_age = False
                if over_version or over_age:
                    entry.tier = MemoryTier.ARCHIVAL
                    marked.append(entry)
        if marked:
            logger.info("Lineage prune: {} superseded versions archived", len(marked))
        return marked

    async def run_decay_pass(
        self,
        entries: list[MemoryEntry],
    ) -> tuple[list[MemoryEntry], list[MemoryEntry]]:
        """Scan entries and classify into archive/forget lists.

        Returns (to_archive, to_forget).
        """
        # 世系裁剪先行：superseded 版本（含 USER 类型）按版本数/保留天数收敛为
        # ARCHIVAL。必须在下方 USER continue 之前独立处理——否则 superseded 的 USER
        # 旧版本会被跳过而永不收敛。active USER 仍照常 continue 不衰减，语义不破。
        self.prune_lineage(entries)
        to_archive: list[MemoryEntry] = []
        to_forget: list[MemoryEntry] = []
        for entry in entries:
            from echo_agent.memory.types import MemoryTier, MemoryType
            # 世系裁剪判定超限的 superseded 旧版本(已翻 ARCHIVAL)必须真正走遗忘删除,
            # 否则磁盘无界堆积。此分支必须在下方 USER continue 之前——superseded 的
            # USER 旧版本才能进 to_forget。只作用于 superseded:active USER 恒非
            # superseded(is_superseded 恒 False),天然不进此分支,衰减语义不破。
            if entry.is_superseded and entry.tier == MemoryTier.ARCHIVAL:
                to_forget.append(entry)
                continue
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
