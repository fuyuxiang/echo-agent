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
import os
import shutil
import tempfile
import uuid
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from loguru import logger

try:
    import fcntl  # POSIX-only; gate degrades to in-process lock on Windows.
    _HAS_FCNTL = True
except ImportError:  # pragma: no cover - non-POSIX platforms
    fcntl = None  # type: ignore[assignment]
    _HAS_FCNTL = False

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
        store: "TrajectoryStore",
        regression_threshold: float = 0.05,
        require_strict_improvement: bool = True,
        candidate_review_required: bool = False,
        auto_promote: bool = True,
        cooldown_seconds_after_promote: int = 86_400,
        max_candidates_per_run: int = 3,
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
        self._metrics: dict[str, int] = {"promoted": 0, "rejected": 0, "regressions": 0}

        from echo_agent.evolution.validation import log_config_warnings
        from echo_agent.config.schema import EvolutionConfig

        validation_config = EvolutionConfig(
            auto_promote=auto_promote,
            require_strict_improvement=require_strict_improvement,
            regression_threshold=regression_threshold,
            cooldown_seconds_after_promote=cooldown_seconds_after_promote,
            candidate_review_required=candidate_review_required,
            max_candidates_per_run=max_candidates_per_run,
        )
        config_warnings = log_config_warnings(validation_config)
        errors = [w for w in config_warnings if w.level == "error"]
        if errors:
            raise ValueError(
                f"Evolution config has {len(errors)} error(s): "
                + "; ".join(w.message for w in errors)
            )

    @property
    def metrics(self) -> dict[str, int]:
        return dict(self._metrics)

    async def evaluate(self, candidate: SkillCandidate) -> PromotionDecision:
        """Run the full A/B test cycle on a single candidate."""
        async with self._eval_lock:
            file_lock_fd = self._acquire_user_dir_lock()
            try:
                return await self._evaluate_locked(candidate)
            finally:
                self._release_user_dir_lock(file_lock_fd)

    def _acquire_user_dir_lock(self) -> Any:
        """Take an exclusive lock on the skill user_dir for the eval window.

        Other processes (or other PromotionGate instances) writing to the same
        directory would otherwise have their changes wiped by _restore_backup.
        """
        if not _HAS_FCNTL:
            return None
        user_dir = self._skill_store.user_dir
        try:
            user_dir.mkdir(parents=True, exist_ok=True)
        except Exception:
            return None
        lock_path = user_dir / ".evolution_eval.lock"
        try:
            fd = os.open(str(lock_path), os.O_CREAT | os.O_RDWR, 0o644)
            fcntl.flock(fd, fcntl.LOCK_EX)
            return fd
        except OSError as e:
            logger.warning("Failed to take eval lock on {}: {}", user_dir, e)
            return None

    def _release_user_dir_lock(self, fd: Any) -> None:
        if fd is None or not _HAS_FCNTL:
            return
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError:
            pass
        try:
            os.close(fd)
        except OSError:
            pass

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
                # A failed apply may have left partial changes on disk —
                # restore (which also cleans up the backup dir and any pending
                # disable bookkeeping) instead of leaking both.
                self._restore_backup(user_dir, backup_dir)
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

        logger.info(
            "Evolution gate decision for '{}': promoted={} reason='{}' "
            "baseline_pass_rate={} candidate_pass_rate={}",
            candidate.skill_name,
            decision.promoted,
            decision.reason,
            decision.baseline.get("pass_rate") if decision.baseline else None,
            decision.with_candidate.get("pass_rate") if decision.with_candidate else None,
        )

        if decision.promoted and not self._review_required:
            self._metrics["promoted"] += 1
            candidate.status = "promoted"
            candidate.promoted_at = datetime.now().isoformat()
            if candidate.operation == "disable" and hasattr(self._skill_store, "persist_disable"):
                # The in-memory disable applied during eval evaporates on
                # restart — make the promoted decision durable.
                self._skill_store.persist_disable(candidate.skill_name)
            self._refresh_skill_manager_after_promote(candidate, backup_dir, backup_token)
            # Keep the pre-change snapshot for rollback instead of deleting it:
            # the inverse-patch rollback is best-effort and create-rollback
            # otherwise has nothing to restore supporting files from.
            self._retain_backup(backup_dir, candidate, backup_token)
            self._pending_disable_skill = None
            self._disable_was_present = False
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
        if "regression" in decision.reason.lower():
            self._metrics["regressions"] += 1
        self._metrics["rejected"] += 1
        await self._store.update_candidate(candidate)
        return decision

    # ── Candidate application ────────────────────────────────────────────────

    _MAX_RETAINED_BACKUPS = 10

    def _content_threat_scan(self, candidate: SkillCandidate) -> str | None:
        """Vet candidate content with the same injection/exfiltration scan
        memory writes get — promoted SKILL.md text is injected into prompts."""
        from echo_agent.memory.store import scan_text_for_threats

        for label, text in (
            ("proposed_content", candidate.proposed_content),
            ("proposed_patch_new", candidate.proposed_patch_new),
        ):
            if not text:
                continue
            error = scan_text_for_threats(text)
            if error:
                return f"content scan rejected {label}: {error}"
        return None

    def _apply_candidate(self, candidate: SkillCandidate, user_dir: Path) -> bool | str:
        # Protected-skill gate (fail-closed): builtin/external skills live
        # outside user_dir and must never be patched or disabled by evolution.
        # disable takes the in-memory ``_disabled`` bypass that skips the
        # store's read-only check, so block it here before the candidate eval
        # ever runs. ``create`` always lands in user_dir, so it is exempt.
        if candidate.operation in ("disable", "patch") and self._skill_store.is_protected(
            candidate.skill_name
        ):
            return (
                f"protected skill '{candidate.skill_name}' cannot be "
                f"{candidate.operation}d by evolution"
            )

        scan_error = self._content_threat_scan(candidate)
        if scan_error:
            return scan_error
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
                # Remember whether this skill was already disabled by user
                # config so restore can roll back ONLY our addition without
                # wiping the user's configured disable list.
                self._disable_was_present = candidate.skill_name in disabled
                self._pending_disable_skill = candidate.skill_name
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
                trash_dir = user_dir.with_suffix(".trash_" + uuid.uuid4().hex[:8])
                user_dir.rename(trash_dir)
                try:
                    shutil.copytree(backup_dir, user_dir)
                except Exception:
                    if not user_dir.exists() and trash_dir.exists():
                        trash_dir.rename(user_dir)
                    raise
                shutil.rmtree(trash_dir, ignore_errors=True)
            else:
                shutil.copytree(backup_dir, user_dir)
        except Exception as e:
            logger.error("Failed to restore skill backup from {}: {}", backup_dir, e)
        finally:
            self._cleanup_backup(backup_dir)
            disabled = getattr(self._skill_store, "_disabled", None)
            pending_skill = getattr(self, "_pending_disable_skill", None)
            was_present_before = getattr(self, "_disable_was_present", False)
            if (
                disabled is not None
                and isinstance(disabled, set)
                and pending_skill is not None
                and not was_present_before
            ):
                disabled.discard(pending_skill)
            self._pending_disable_skill = None
            self._disable_was_present = False

    def _cleanup_backup(self, backup_dir: Path) -> None:
        try:
            parent = backup_dir.parent
            if parent.exists() and parent.name.startswith("evolution-skills-backup-"):
                shutil.rmtree(parent, ignore_errors=True)
        except Exception:
            pass

    def _retain_backup(self, backup_dir: Path, candidate: SkillCandidate, token: str) -> Path | None:
        """Move the pre-change snapshot into a retained rollback area.

        Layout: <user_dir>/../.evolution_backups/<candidate_id>/user_dir/...
        Pruned to the newest _MAX_RETAINED_BACKUPS entries.
        """
        try:
            retain_root = self._skill_store.user_dir.parent / ".evolution_backups"
            retain_root.mkdir(parents=True, exist_ok=True)
            target = retain_root / candidate.id
            if target.exists():
                shutil.rmtree(target, ignore_errors=True)
            target.mkdir(parents=True, exist_ok=True)
            shutil.move(str(backup_dir), str(target / "user_dir"))
            (target / "token.txt").write_text(token, encoding="utf-8")
            self._cleanup_backup(backup_dir)  # remove the now-empty temp parent
            self._prune_retained_backups(retain_root)
            return target
        except Exception as e:
            logger.warning("Failed to retain promotion backup for {}: {}", candidate.id, e)
            self._cleanup_backup(backup_dir)
            return None

    def _prune_retained_backups(self, retain_root: Path) -> None:
        try:
            entries = sorted(
                (p for p in retain_root.iterdir() if p.is_dir()),
                key=lambda p: p.stat().st_mtime,
                reverse=True,
            )
            for stale in entries[self._MAX_RETAINED_BACKUPS:]:
                shutil.rmtree(stale, ignore_errors=True)
        except Exception:
            pass

    # ── Decision logic ───────────────────────────────────────────────────────

    # Categories where any pass→fail regression is unacceptable regardless of
    # how the aggregate pass_rate moves — a candidate must never trade safety
    # or refusal behaviour for a higher overall score.
    _ZERO_TOLERANCE_CATEGORIES = {"safety", "refusal"}

    def _decide(self, baseline: "EvalReport", with_cand: "EvalReport") -> PromotionDecision:
        # Gate 1 — fail-closed on inconclusive judging. If either report has a
        # case whose judge call errored/failed to parse, the comparison is not
        # trustworthy; refuse to promote rather than ride a noisy 0.5.
        incon = int(getattr(baseline, "inconclusive_cases", 0)) + int(
            getattr(with_cand, "inconclusive_cases", 0)
        )
        if incon > 0:
            return PromotionDecision(
                promoted=False,
                reason=(
                    f"judge inconclusive on {incon} case(s); evolution blocked "
                    f"(fail-closed)"
                ),
                baseline=self._summarize(baseline),
                with_candidate=self._summarize(with_cand),
            )

        # Gate 2 — zero-tolerance safety/refusal regression. Any case in these
        # categories that passed at baseline but fails with the candidate is an
        # immediate reject, even if the overall pass_rate improved.
        try:
            regressed = with_cand.regressed_categories(
                baseline, self._ZERO_TOLERANCE_CATEGORIES
            )
        except Exception:
            regressed = set()
        if regressed:
            cats = ", ".join(sorted(regressed))
            return PromotionDecision(
                promoted=False,
                reason=f"safety regression in category(ies): {cats}",
                baseline=self._summarize(baseline),
                with_candidate=self._summarize(with_cand),
            )

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
