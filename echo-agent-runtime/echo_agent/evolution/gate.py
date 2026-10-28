"""PromotionGate — A/B verification before any skill change reaches production.

Workflow for a candidate:

  1. Run the baseline eval dataset against the *current* skill library.
  2. Snapshot the user skills directory into an isolated candidate library.
  3. Apply the candidate's change only to that isolated library.
  4. Run the same eval dataset again.
  5. Compare reports:
       - regression beyond ``regression_threshold`` → reject
       - strict improvement → apply the verified change to the live library
       - tie (when ``require_strict_improvement``) → reject
  6. Persist the candidate's eval reports + status into the TrajectoryStore.

Evaluation never applies an unapproved candidate to the live library. Cancelling
the evaluation or losing the process therefore cannot leave a candidate online.
"""

from __future__ import annotations

import asyncio
import hashlib
import inspect
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
from echo_agent.evolution.validation import validate_candidate_identity

if TYPE_CHECKING:
    from echo_agent.evaluation.dataset import EvalDataset
    from echo_agent.evaluation.runner import EvalReport, EvalRunner
    from echo_agent.evolution.store import TrajectoryStore


# Production factories accept ``skill_store=`` so the baseline/candidate
# snapshots are bound before constructing a runtime. Legacy no-argument
# factories remain accepted for integrations using deterministic evaluators.
EvalRunnerFactory = Callable[..., "EvalRunner"]


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
        store: "TrajectoryStore",
        regression_threshold: float = 0.05,
        require_strict_improvement: bool = True,
        candidate_review_required: bool = False,
        auto_promote: bool = True,
        cooldown_seconds_after_promote: int = 86_400,
        max_candidates_per_run: int = 3,
        min_eval_cases: int = 3,
    ):
        self._make_runner = eval_runner_factory
        self._load_dataset = eval_dataset_loader
        self._skill_store = skill_store
        self._store = store
        self._regression_threshold = float(regression_threshold)
        self._require_strict = bool(require_strict_improvement)
        self._review_required = bool(candidate_review_required)
        self._min_eval_cases = int(min_eval_cases)
        self._eval_lock = asyncio.Lock()
        self._metrics: dict[str, int] = {"promoted": 0, "rejected": 0, "regressions": 0}
        self._evaluation_skill_store: SkillStore | None = None

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
            # LOCK_NB plus an async retry avoids blocking the event loop when
            # another gate/process is holding the same library lock.
            while True:
                try:
                    file_lock_fd = self._acquire_user_dir_lock()
                    break
                except BlockingIOError:
                    await asyncio.sleep(0.05)
            try:
                return await self._evaluate_locked(candidate)
            except asyncio.CancelledError:
                if candidate.status == "evaluating":
                    candidate.status = "rejected"
                    candidate.rejected_reason = "evaluation cancelled; live skills unchanged"
                    await self._store.update_candidate(candidate)
                raise
            finally:
                self._evaluation_skill_store = None
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
        except OSError as error:
            raise RuntimeError(f"Cannot prepare skill evaluation lock: {error}") from error
        lock_path = user_dir / ".evolution_eval.lock"
        try:
            fd = os.open(str(lock_path), os.O_CREAT | os.O_RDWR, 0o644)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fd
        except BlockingIOError:
            os.close(fd)
            raise
        except OSError as e:
            if "fd" in locals():
                os.close(fd)
            raise RuntimeError(f"Failed to take eval lock on {user_dir}: {e}") from e

    def _release_user_dir_lock(self, fd: Any) -> None:
        if fd is None or not _HAS_FCNTL:
            return
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError:
            # Closing this process-owned descriptor below also releases its flock;
            # an explicit unlock failure therefore cannot leave the lock held.
            pass
        try:
            os.close(fd)
        except OSError:
            # Release is idempotent and the descriptor may already have been
            # closed by an earlier teardown path.
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

        user_dir = self._skill_store.user_dir
        if candidate.operation == "disable" and not hasattr(self._skill_store, "_disabled"):
            return await self._reject(candidate, "skill_store has no _disabled attribute")
        initial_fingerprint = self._library_fingerprint(self._skill_store)
        with tempfile.TemporaryDirectory(prefix="evolution-eval-") as staging:
            staged_dir = Path(staging) / "skills"
            from echo_agent.evaluation.runtime import copy_skill_tree

            try:
                copy_skill_tree(user_dir, staged_dir)
                builtin_dir = self._skill_store._builtin_dir
                if builtin_dir is not None and builtin_dir.exists():
                    copy_skill_tree(builtin_dir, Path(staging) / "builtin_skills")
                    builtin_dir = Path(staging) / "builtin_skills"
                external_dirs = []
                for index, source in enumerate(self._skill_store._external_dirs):
                    if source.exists():
                        target = Path(staging) / f"external_skills_{index}"
                        copy_skill_tree(source, target)
                        external_dirs.append(target)
            except (OSError, ValueError) as error:
                return await self._reject(candidate, f"skill snapshot failed: {error}")
            staged = SkillStore(
                user_dir=staged_dir,
                builtin_dir=builtin_dir,
                external_dirs=external_dirs,
                disabled=list(self._skill_store._disabled),
            )
            self._evaluation_skill_store = staged
            try:
                baseline_report = await self._run_eval(dataset)
            except Exception as e:
                return await self._reject(candidate, f"baseline eval failed: {e}")
            candidate.eval_baseline = self._summarize(baseline_report)

            applied = self._apply_candidate(candidate, staged_dir)
            if applied is not True:
                return await self._reject(candidate, str(applied or "could not apply candidate"))
            try:
                with_report = await self._run_eval(dataset)
            except Exception as e:
                return await self._reject(candidate, f"candidate eval failed: {e}")
            candidate.eval_with_candidate = self._summarize(with_report)
            decision = self._decide(baseline_report, with_report)
            self._evaluation_skill_store = None

            if not decision.promoted:
                if "regression" in decision.reason.lower():
                    self._metrics["regressions"] += 1
                return await self._reject(candidate, decision.reason)
            if self._review_required:
                candidate.status = "needs_review"
                candidate.rejected_reason = "candidate_review_required: held for human approval"
                await self._store.update_candidate(candidate)
                return PromotionDecision(False, candidate.rejected_reason,
                                         candidate.eval_baseline, candidate.eval_with_candidate)

            # A user may legitimately edit skills while a long eval runs. Never
            # overwrite that edit, or promote against a library we did not test.
            if self._library_fingerprint(self._skill_store) != initial_fingerprint:
                return await self._reject(candidate, "live skill library changed during evaluation; retry required")
            backup_dir, backup_token = self._snapshot_user_dir(user_dir)
            applied = self._apply_candidate(candidate, user_dir)
            if applied is not True:
                self._cleanup_backup(backup_dir)
                return await self._reject(candidate, str(applied or "promotion apply failed"))
            if candidate.operation == "disable":
                self._skill_store.persist_disable(candidate.skill_name)
            self._retain_backup(backup_dir, candidate, backup_token)
            self._pending_disable_skill = None
            self._disable_was_present = False
            candidate.status = "promoted"
            candidate.promoted_at = datetime.now().isoformat()
            await self._store.update_candidate(candidate)
            self._metrics["promoted"] += 1
            logger.info("Evolution promoted '{}' after isolated evaluation: {}", candidate.skill_name, decision.reason)
            return decision

    async def _reject(self, candidate: SkillCandidate, reason: str) -> PromotionDecision:
        candidate.status = "rejected"
        candidate.rejected_reason = reason
        self._metrics["rejected"] += 1
        await self._store.update_candidate(candidate)
        return PromotionDecision(False, reason, candidate.eval_baseline, candidate.eval_with_candidate)

    @staticmethod
    def _library_fingerprint(store: SkillStore) -> str:
        digest = hashlib.sha256()
        digest.update(repr(sorted(store._disabled)).encode())
        # Hash supporting files as well as SKILL.md. Ignore the advisory lock
        # (which contains no library content and is owned by this gate).
        for root, _writable in store._all_roots():
            digest.update(str(root).encode())
            for path in sorted(root.rglob("*")):
                if path.is_file() and path.name != ".evolution_eval.lock":
                    digest.update(str(path.relative_to(root)).encode())
                    digest.update(path.read_bytes())
        return digest.hexdigest()

    # ── Candidate application ────────────────────────────────────────────────

    _MAX_RETAINED_BACKUPS = 10

    def _content_threat_scan(self, candidate: SkillCandidate) -> str | None:
        """Reject instructions in candidate documentation before promotion."""
        from echo_agent.memory.store import scan_document_for_threats

        for label, text in (
            ("proposed_content", candidate.proposed_content),
            ("proposed_patch_new", candidate.proposed_patch_new),
        ):
            if not text:
                continue
            error, _warnings = scan_document_for_threats(text)
            if error:
                return f"content scan rejected {label}: {error}"
        return None

    def _apply_candidate(self, candidate: SkillCandidate, user_dir: Path) -> bool | str:
        skill_store = self._evaluation_skill_store or self._skill_store
        # A user directory takes precedence over builtins. Letting a candidate
        # create ``foo/SKILL.md`` with ``name: protected`` (or rename a patched
        # skill) would shadow a protected skill without ever writing its path.
        identity_error = validate_candidate_identity(candidate, skill_store)
        if identity_error:
            return identity_error
        # Protected-skill gate (fail-closed): builtin/external skills live
        # outside user_dir and must never be patched or disabled by evolution.
        # disable takes the in-memory ``_disabled`` bypass that skips the
        # store's read-only check, so block it here before the candidate eval
        # ever runs. ``create`` always lands in user_dir, so it is exempt.
        if candidate.operation in ("disable", "patch") and skill_store.is_protected(
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
                err = skill_store.create_skill(
                    candidate.skill_name,
                    candidate.proposed_content,
                )
                if err:
                    return f"create failed: {err}"
                return True

            if candidate.operation == "patch":
                err = skill_store.patch_skill(
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
                disabled = getattr(skill_store, "_disabled", None)
                if disabled is None:
                    return "skill_store has no _disabled attribute"
                # Remember whether this skill was already disabled by user
                # config so restore can roll back ONLY our addition without
                # wiping the user's configured disable list.
                self._disable_was_present = candidate.skill_name in disabled
                self._pending_disable_skill = candidate.skill_name
                disabled.update(skill_store.resolved_names(candidate.skill_name))
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
            # Backup cleanup must not change an already-decided promotion result.
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
            # A failed retention sweep leaves extra backups available for recovery.
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

        # Gate 3 — minimum sample size. A single eval case can flip pass_rate
        # from 0.0 to 1.0; treating that as a real improvement lets noise drive
        # promotion. Below the configured floor the comparison is inconclusive.
        # Count only conclusive cases — total_cases includes inconclusive ones,
        # which carry no signal. Gate 1 already fails closed on any inconclusive
        # case today, but gating on the conclusive count keeps Gate 3 correct
        # independent of Gate 1's policy.
        conclusive_cases = (
            int(getattr(with_cand, "total_cases", 0))
            - int(getattr(with_cand, "inconclusive_cases", 0))
        )
        if conclusive_cases < self._min_eval_cases:
            return PromotionDecision(
                promoted=False,
                reason=(
                    f"inconclusive: only {conclusive_cases} conclusive cases "
                    f"(min {self._min_eval_cases})"
                ),
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
        try:
            inspect.signature(self._make_runner).bind(skill_store=self._evaluation_skill_store)
        except TypeError:
            runner = self._make_runner()
        else:
            runner = self._make_runner(skill_store=self._evaluation_skill_store)
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

    # _refresh_skill_manager_after_promote used to live here. It refreshed a
    # second, parallel skill model (SkillManager, with its own manifest.json /
    # .status files) that production never constructed — app.py passed
    # skill_manager=None unconditionally, and no manifest.json ever existed on
    # disk. SkillStore reads from disk on every lookup, so there is no cache to
    # invalidate after a promotion.
