"""PromotionGate — A/B verification before any skill change reaches production.

Workflow for a candidate:

  1. Run the baseline eval dataset against the *current* skill library.
  2. Snapshot the user skills directory to a temp backup.
  3. Apply the candidate's change in-place on user skills.
  4. Run the same eval dataset again.
  5. Compare reports:
       - regression beyond ``regression_threshold`` → reject + restore
       - strict improvement → promote (keep applied change)
       - tie (when ``require_strict_improvement``) → reject + restore
  6. Persist the candidate's eval reports + status into the TrajectoryStore.

The gate never mutates the SkillStore object itself — it works exclusively
through the on-disk user directory, which the SkillStore reads on every
call. This keeps the AgentLoop's references intact and makes a failure
recovery a simple directory move.
"""

from __future__ import annotations

import asyncio
import shutil
import tempfile
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from loguru import logger

from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.store import SkillStore

if TYPE_CHECKING:
    from echo_agent.evaluation.dataset import EvalDataset
    from echo_agent.evaluation.runner import EvalReport, EvalRunner
    from echo_agent.evolution.store import TrajectoryStore
    from echo_agent.skills.manager import SkillManager


EvalRunnerFactory = Callable[[], "EvalRunner"]


@dataclass
class PromotionDecision:
    promoted: bool
    reason: str
    baseline: dict[str, Any] | None
    with_candidate: dict[str, Any] | None


class PromotionGate:
    """A/B-tests every candidate against the baseline eval dataset."""

    def __init__(
        self,
        *,
        eval_runner_factory: EvalRunnerFactory,
        eval_dataset_loader: Callable[[], Awaitable["EvalDataset"]] | Callable[[], "EvalDataset"],
        skill_store: SkillStore,
        skill_manager: "SkillManager | None",
        store: TrajectoryStore,
        regression_threshold: float = 0.05,
        require_strict_improvement: bool = True,
        candidate_review_required: bool = False,
    ):
        self._make_runner = eval_runner_factory
        self._load_dataset = eval_dataset_loader
        self._skill_store = skill_store
        self._skill_manager = skill_manager
        self._store = store
        self._regression_threshold = float(regression_threshold)
        self._require_strict = bool(require_strict_improvement)
        self._review_required = bool(candidate_review_required)
        self._eval_lock = asyncio.Lock()

    async def evaluate(self, candidate: SkillCandidate) -> PromotionDecision:
        """Run the full A/B test cycle on a single candidate."""
        async with self._eval_lock:
            return await self._evaluate_locked(candidate)

    async def _evaluate_locked(self, candidate: SkillCandidate) -> PromotionDecision:
        candidate.status = "evaluating"
        await self._store.update_candidate(candidate)

        dataset = await self._resolve_dataset()
        if dataset is None or not getattr(dataset, "cases", None):
            candidate.status = "rejected"
            candidate.rejected_reason = "baseline eval dataset is empty or missing"
            await self._store.update_candidate(candidate)
            return PromotionDecision(
                promoted=False,
                reason=candidate.rejected_reason,
                baseline=None,
                with_candidate=None,
            )

        # 1. baseline
        try:
            baseline_report = await self._run_eval(dataset)
        except Exception as e:
            candidate.status = "rejected"
            candidate.rejected_reason = f"baseline eval failed: {e}"
            await self._store.update_candidate(candidate)
            return PromotionDecision(
                promoted=False,
                reason=candidate.rejected_reason,
                baseline=None,
                with_candidate=None,
            )
        candidate.eval_baseline = self._summarize(baseline_report)

        # 2. snapshot
        user_dir = self._skill_store.user_dir
        backup_dir, backup_token = self._snapshot_user_dir(user_dir)

        try:
            applied = self._apply_candidate(candidate, user_dir)
            if applied is not True:
                reason = applied or "could not apply candidate"
                candidate.status = "rejected"
                candidate.rejected_reason = reason
                candidate.eval_with_candidate = None
                await self._store.update_candidate(candidate)
                return PromotionDecision(
                    promoted=False,
                    reason=reason,
                    baseline=candidate.eval_baseline,
                    with_candidate=None,
                )

            with_report = await self._run_eval(dataset)
        except Exception as e:
            self._restore_backup(user_dir, backup_dir)
            candidate.status = "rejected"
            candidate.rejected_reason = f"candidate eval failed: {e}"
            await self._store.update_candidate(candidate)
            return PromotionDecision(
                promoted=False,
                reason=candidate.rejected_reason,
                baseline=candidate.eval_baseline,
                with_candidate=None,
            )

        candidate.eval_with_candidate = self._summarize(with_report)

        decision = self._decide(baseline_report, with_report)

        if decision.promoted and not self._review_required:
            candidate.status = "promoted"
            candidate.promoted_at = datetime.now().isoformat()
            self._refresh_skill_manager_after_promote(candidate, backup_dir, backup_token)
            self._cleanup_backup(backup_dir)
            await self._store.update_candidate(candidate)
            return decision

        if decision.promoted and self._review_required:
            # Even though the gate would have promoted, hold the change for
            # human sign-off. Restore the on-disk state and store the eval
            # results for ``echo-agent evolution list-candidates``.
            self._restore_backup(user_dir, backup_dir)
            candidate.status = "needs_review"
            candidate.rejected_reason = "candidate_review_required: held for human approval"
            await self._store.update_candidate(candidate)
            return PromotionDecision(
                promoted=False,
                reason=candidate.rejected_reason,
                baseline=candidate.eval_baseline,
                with_candidate=candidate.eval_with_candidate,
            )

        # rejected — restore
        self._restore_backup(user_dir, backup_dir)
        candidate.status = "rejected"
        candidate.rejected_reason = decision.reason
        await self._store.update_candidate(candidate)
        return decision

    # ── Candidate application ────────────────────────────────────────────────

    def _apply_candidate(self, candidate: SkillCandidate, user_dir: Path) -> bool | str:
        try:
            if candidate.operation == "create":
                err = self._skill_store.create_skill(
                    candidate.skill_name,
                    candidate.proposed_content,
                )
                if err:
                    return f"create failed: {err}"
                return True

            if candidate.operation == "patch":
                err = self._skill_store.patch_skill(
                    candidate.skill_name,
                    candidate.proposed_patch_old,
                    candidate.proposed_patch_new,
                )
                if err:
                    return f"patch failed: {err}"
                return True

            if candidate.operation == "disable":
                # Mark the skill as disabled in this SkillStore instance for the
                # duration of the eval run. ``_disabled`` is consulted in
                # ``list_all`` so the eval will run as if the skill is gone.
                disabled = getattr(self._skill_store, "_disabled", None)
                if disabled is None:
                    return "skill_store has no _disabled attribute"
                disabled.add(candidate.skill_name)
                return True

            return f"unknown operation '{candidate.operation}'"
        except Exception as e:
            return f"apply raised: {e}"

    # ── Snapshot / restore ───────────────────────────────────────────────────

    def _snapshot_user_dir(self, user_dir: Path) -> tuple[Path, str]:
        user_dir.mkdir(parents=True, exist_ok=True)
        token = datetime.now().strftime("%Y%m%dT%H%M%S%f")
        backup_root = Path(tempfile.mkdtemp(prefix="evolution-skills-backup-"))
        backup_dir = backup_root / "user_dir"
        shutil.copytree(user_dir, backup_dir, dirs_exist_ok=False)
        return backup_dir, token

    def _restore_backup(self, user_dir: Path, backup_dir: Path) -> None:
        try:
            if user_dir.exists():
                shutil.rmtree(user_dir)
            shutil.copytree(backup_dir, user_dir)
        except Exception as e:
            logger.error("Failed to restore skill backup from {}: {}", backup_dir, e)
        finally:
            self._cleanup_backup(backup_dir)
            # Also clear any in-memory disable flag set by `apply` for disable ops.
            disabled = getattr(self._skill_store, "_disabled", None)
            if disabled is not None and isinstance(disabled, set):
                disabled.clear()

    def _cleanup_backup(self, backup_dir: Path) -> None:
        try:
            parent = backup_dir.parent
            if parent.exists() and parent.name.startswith("evolution-skills-backup-"):
                shutil.rmtree(parent, ignore_errors=True)
        except Exception:
            pass

    # ── Decision logic ───────────────────────────────────────────────────────

    def _decide(self, baseline: "EvalReport", with_cand: "EvalReport") -> PromotionDecision:
        b_pass = float(baseline.pass_rate)
        c_pass = float(with_cand.pass_rate)
        b_score = float(baseline.avg_score)
        c_score = float(with_cand.avg_score)

        if c_pass < b_pass - self._regression_threshold:
            return PromotionDecision(
                promoted=False,
                reason=(
                    f"regression: pass_rate dropped {b_pass:.3f} → {c_pass:.3f} "
                    f"(threshold {self._regression_threshold:.3f})"
                ),
                baseline=self._summarize(baseline),
                with_candidate=self._summarize(with_cand),
            )

        improvement_pass = c_pass > b_pass
        improvement_score = c_score > b_score

        if self._require_strict:
            if improvement_pass and (c_score >= b_score - 1e-6):
                reason = (
                    f"improvement: pass_rate {b_pass:.3f} → {c_pass:.3f}, "
                    f"avg_score {b_score:.3f} → {c_score:.3f}"
                )
                return PromotionDecision(True, reason, self._summarize(baseline), self._summarize(with_cand))
            if not improvement_pass and improvement_score and c_pass >= b_pass:
                reason = (
                    f"improvement: avg_score {b_score:.3f} → {c_score:.3f} (pass_rate stable)"
                )
                return PromotionDecision(True, reason, self._summarize(baseline), self._summarize(with_cand))
            return PromotionDecision(
                promoted=False,
                reason=(
                    f"no strict improvement: pass_rate {b_pass:.3f} → {c_pass:.3f}, "
                    f"avg_score {b_score:.3f} → {c_score:.3f}"
                ),
                baseline=self._summarize(baseline),
                with_candidate=self._summarize(with_cand),
            )

        # Loose policy: promote when not a regression.
        return PromotionDecision(
            promoted=True,
            reason=(
                f"non-regression: pass_rate {b_pass:.3f} → {c_pass:.3f}, "
                f"avg_score {b_score:.3f} → {c_score:.3f}"
            ),
            baseline=self._summarize(baseline),
            with_candidate=self._summarize(with_cand),
        )

    # ── Eval execution ───────────────────────────────────────────────────────

    async def _run_eval(self, dataset: "EvalDataset") -> "EvalReport":
        runner = self._make_runner()
        return await runner.run_dataset(dataset)

    async def _resolve_dataset(self) -> "EvalDataset | None":
        try:
            value = self._load_dataset()
            if asyncio.iscoroutine(value):
                value = await value
            return value
        except Exception as e:
            logger.warning("Failed to load baseline eval dataset: {}", e)
            return None

    @staticmethod
    def _summarize(report: "EvalReport") -> dict[str, Any]:
        try:
            base = report.summary()
        except Exception:
            base = {}
        return {
            "pass_rate": float(getattr(report, "pass_rate", 0.0)),
            "avg_score": float(getattr(report, "avg_score", 0.0)),
            "total_cases": int(getattr(report, "total_cases", 0)),
            "passed_cases": int(getattr(report, "passed_cases", 0)),
            "duration_ms": float(getattr(report, "duration_ms", 0.0)),
            **{k: v for k, v in base.items() if k not in {"total", "passed", "pass_rate", "avg_score", "duration_ms"}},
        }

    def _refresh_skill_manager_after_promote(
        self,
        candidate: SkillCandidate,
        backup_dir: Path,
        backup_token: str,
    ) -> None:
        """Update SkillManager bookkeeping if available — non-fatal."""
        if self._skill_manager is None:
            return
        try:
            # SkillManager scans on init; refresh in-memory map.
            if hasattr(self._skill_manager, "_load_installed"):
                self._skill_manager._load_installed()
        except Exception as e:
            logger.debug("SkillManager refresh after promote failed: {}", e)
