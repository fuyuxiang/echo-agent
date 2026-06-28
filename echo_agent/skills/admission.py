"""SkillAdmission — always-on governance gate between skill candidates and disk.

Independent of evolution.enabled: reviewer/evolver both feed SkillCandidate here.
Routes by risk + 3-tier policy to either auto-write or stage-for-review. Any
apply failure degrades to staged — a candidate is never silently dropped.
"""

from __future__ import annotations

import difflib
from dataclasses import dataclass
from typing import Literal

from loguru import logger

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate, _now_iso
from echo_agent.memory.store import scan_text_for_threats
from echo_agent.skills.store import SkillStore


@dataclass
class AdmissionResult:
    outcome: Literal["written", "staged", "rejected"]
    candidate_id: str
    message: str
    diff: str = ""


class SkillAdmission:
    def __init__(
        self,
        *,
        skill_store: SkillStore,
        candidate_store: TrajectoryStore,
        policy: str,
        auto_write_risk: str,
    ):
        self._skills = skill_store
        self._candidates = candidate_store
        self._policy = policy
        self._auto_write_risk = auto_write_risk

    @staticmethod
    def make_diff(old: str, new: str, name: str) -> str:
        return "".join(
            difflib.unified_diff(
                old.splitlines(keepends=True),
                new.splitlines(keepends=True),
                fromfile=f"a/{name}",
                tofile=f"b/{name}",
            )
        )

    def _scan_text(self, c: SkillCandidate) -> str:
        return " ".join(
            s for s in (c.proposed_content, c.proposed_patch_new) if s
        ).strip()

    def _should_write(self, c: SkillCandidate) -> bool:
        if c.operation == "delete":
            # delete 永远走暂存,即便 auto_write
            return False
        if self._policy == "manual_only":
            return False
        if self._policy == "stage_for_review":
            return c.risk == "low"
        # auto_write: risk 不高于 auto_write_risk 即写
        if self._auto_write_risk == "high":
            return True
        return c.risk == "low"

    def _apply(self, c: SkillCandidate) -> None:
        """Write the candidate to disk via SkillStore. Raises on store error."""
        if c.operation in ("create",):
            err = self._skills.create_skill(c.skill_name, c.proposed_content)
        elif c.operation in ("patch",):
            err = self._skills.patch_skill(
                c.skill_name, c.proposed_patch_old, c.proposed_patch_new
            )
        elif c.operation in ("disable",):
            self._skills.persist_disable(c.skill_name)
            err = None
        elif c.operation == "delete":
            err = self._skills.delete_skill(c.skill_name)
        else:
            err = f"unknown operation '{c.operation}'"
        if err:
            raise RuntimeError(err)
        # provenance 镜像:失败只 warning,SQLite 为权威源
        if c.operation in ("create", "patch"):
            mirror_err = self._skills.write_provenance(
                c.skill_name,
                source=c.source,
                created_at=c.created_at,
                promotion_status="active",
                created_from_session=c.created_from_session,
            )
            if mirror_err:
                logger.warning("Provenance mirror failed for '{}': {}",
                               c.skill_name, mirror_err)

    async def admit(self, candidate: SkillCandidate) -> AdmissionResult:
        c = candidate
        # 1) 注入扫描
        to_scan = self._scan_text(c)
        if to_scan:
            threat = scan_text_for_threats(to_scan)
            if threat:
                c.status = "rejected"
                c.rejected_reason = f"injection scan: {threat}"
                await self._candidates.save_candidate(c)
                logger.warning("skill admission rejected: name={} reason={}",
                               c.skill_name, threat)
                return AdmissionResult("rejected", c.id, c.rejected_reason)

        # 2) diff(用于审计/审批展示)
        diff = ""
        if c.operation == "patch":
            diff = self.make_diff(c.proposed_patch_old, c.proposed_patch_new, c.skill_name)
        elif c.operation == "create":
            diff = self.make_diff("", c.proposed_content, c.skill_name)

        # 3) 路由
        if self._should_write(c):
            try:
                self._apply(c)
                c.status = "promoted"
                c.promotion_status = "active"
                c.promoted_at = _now_iso()
                await self._candidates.save_candidate(c)
                logger.info("skill admission written: op={} name={}",
                            c.operation, c.skill_name)
                return AdmissionResult("written", c.id, "applied", diff)
            except Exception as e:
                # 降级:候选不丢,落 staged
                c.status = "needs_review"
                c.promotion_status = "staged"
                c.rejected_reason = f"apply failed, staged: {e}"
                await self._candidates.save_candidate(c)
                logger.warning("skill admission apply failed, staged: name={} err={}",
                               c.skill_name, e)
                return AdmissionResult("staged", c.id, c.rejected_reason, diff)

        c.status = "needs_review"
        c.promotion_status = "staged"
        await self._candidates.save_candidate(c)
        logger.info("skill admission staged: op={} name={}", c.operation, c.skill_name)
        return AdmissionResult("staged", c.id, "staged for review", diff)

    async def list_staged(self, *, limit: int = 100) -> list[SkillCandidate]:
        return await self._candidates.list_candidates(
            status="needs_review", limit=limit
        )

    async def approve(self, candidate_id: str) -> AdmissionResult:
        c = await self._candidates.get_candidate(candidate_id)
        if c is None:
            return AdmissionResult("rejected", candidate_id, "candidate not found")
        if c.status != "needs_review":
            return AdmissionResult("rejected", candidate_id,
                                   f"candidate is '{c.status}', not staged")
        try:
            self._apply(c)
        except Exception as e:
            return AdmissionResult("rejected", candidate_id, f"apply failed: {e}")
        c.status = "promoted"
        c.promotion_status = "active"
        c.promoted_at = _now_iso()
        await self._candidates.save_candidate(c)
        logger.info("skill admission approved: name={}", c.skill_name)
        return AdmissionResult("written", candidate_id, "approved and applied")

    async def reject(self, candidate_id: str, reason: str = "") -> AdmissionResult:
        c = await self._candidates.get_candidate(candidate_id)
        if c is None:
            return AdmissionResult("rejected", candidate_id, "candidate not found")
        c.status = "rejected"
        c.rejected_reason = reason or "rejected by operator"
        await self._candidates.save_candidate(c)
        logger.info("skill admission rejected by operator: name={}", c.skill_name)
        return AdmissionResult("rejected", candidate_id, c.rejected_reason)
