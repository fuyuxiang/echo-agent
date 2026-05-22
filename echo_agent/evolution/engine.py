"""EvolutionEngine — orchestrates the full self-evolution loop."""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from loguru import logger

from echo_agent.evolution.evolver import Evolver
from echo_agent.evolution.gate import EvalRunnerFactory, PromotionGate
from echo_agent.evolution.recorder import TrajectoryRecorder
from echo_agent.evolution.scheduler import EvolutionScheduler
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import (
    EvolutionRun,
    EvolutionTrigger,
    SkillCandidate,
    Trajectory,
)

if TYPE_CHECKING:
    from echo_agent.config.schema import EvolutionConfig
    from echo_agent.evaluation.dataset import EvalDataset
    from echo_agent.models.provider import LLMProvider
    from echo_agent.plugins.hooks import HookRegistry
    from echo_agent.skills.manager import SkillManager
    from echo_agent.skills.store import SkillStore
    from echo_agent.storage.backend import StorageBackend


@dataclass
class _Cooldown:
    skill_name: str
    until_ts: float


class EvolutionEngine:
    """Top-level coordinator for the self-evolving skill harness."""

    def __init__(
        self,
        *,
        config: "EvolutionConfig",
        workspace: Path,
        storage: "StorageBackend",
        provider: "LLMProvider",
        skill_store: "SkillStore",
        skill_manager: "SkillManager | None",
        eval_runner_factory: EvalRunnerFactory,
        eval_dataset_loader: Callable[[], Awaitable["EvalDataset"]] | Callable[[], "EvalDataset"],
        hooks: "HookRegistry | None" = None,
        reflection: Any = None,
    ):
        self._config = config
        self._workspace = workspace
        self._storage = storage
        self._provider = provider
        self._skill_store = skill_store
        self._skill_manager = skill_manager
        self._eval_dataset_loader = eval_dataset_loader

        self._store = TrajectoryStore(storage)
        self._recorder = TrajectoryRecorder(
            self._store,
            redact_args=config.redact_args,
            skill_store=skill_store,
            reflection=reflection,
        )
        self._evolver = Evolver(
            provider=provider,
            store=self._store,
            skill_store=skill_store,
            model=config.evolver_model,
            max_candidates=config.max_candidates_per_run,
            skill_size_limit_bytes=config.skill_size_limit_bytes,
        )
        self._gate = PromotionGate(
            eval_runner_factory=eval_runner_factory,
            eval_dataset_loader=eval_dataset_loader,
            skill_store=skill_store,
            skill_manager=skill_manager,
            store=self._store,
            regression_threshold=config.regression_threshold,
            require_strict_improvement=config.require_strict_improvement,
            candidate_review_required=config.candidate_review_required,
        )
        self._scheduler = EvolutionScheduler(
            run_fn=self.run_evolution,
            unconsumed_count_fn=self._store.count_unconsumed,
            trigger_mode=config.trigger_mode,
            threshold=config.threshold_trajectories,
            cron_expression=config.cron_expression,
        )

        self._hooks = hooks
        self._run_lock = asyncio.Lock()
        self._started = False
        self._cooldowns: dict[str, _Cooldown] = {}

    # ── Lifecycle ────────────────────────────────────────────────────────────

    @property
    def store(self) -> TrajectoryStore:
        return self._store

    @property
    def recorder(self) -> TrajectoryRecorder:
        return self._recorder

    async def start(self) -> None:
        if self._started:
            return
        await self._store.init_schema()
        if self._config.record_trajectories and self._hooks is not None:
            self._recorder.attach(self._hooks)
        if self._config.trajectory_retention_days > 0:
            try:
                purged = await self._store.purge_older_than(
                    self._config.trajectory_retention_days,
                )
                if purged:
                    logger.info("Purged {} old trajectories", purged)
            except Exception as e:
                logger.debug("Trajectory retention purge failed: {}", e)
        await self._scheduler.start()
        self._started = True
        logger.info(
            "EvolutionEngine started (trigger={}, record={}, dataset={})",
            self._config.trigger_mode,
            self._config.record_trajectories,
            self._config.eval_dataset_path,
        )

    async def stop(self) -> None:
        if not self._started:
            return
        await self._scheduler.stop()
        if self._hooks is not None:
            self._recorder.detach(self._hooks)
        self._started = False

    # ── Public API ───────────────────────────────────────────────────────────

    async def run_evolution(
        self,
        *,
        trigger: EvolutionTrigger = "manual",
    ) -> EvolutionRun:
        async with self._run_lock:
            return await self._run_locked(trigger=trigger)

    async def list_recent_runs(self, *, limit: int = 10) -> list[EvolutionRun]:
        return await self._store.list_runs(limit=limit)

    async def list_candidates(
        self,
        *,
        status: str | None = None,
        limit: int = 50,
    ) -> list[SkillCandidate]:
        return await self._store.list_candidates(status=status, limit=limit)

    async def rollback_skill(self, skill_name: str) -> tuple[bool, str]:
        """Roll back the last promoted change for a given skill."""
        candidate = await self._store.latest_promoted_for_skill(skill_name)
        if candidate is None:
            return False, f"no promoted candidate found for skill '{skill_name}'"
        try:
            if candidate.operation == "create":
                err = self._skill_store.delete_skill(skill_name)
                if err:
                    return False, f"delete failed: {err}"
            elif candidate.operation == "patch":
                # Re-apply the inverse patch. If the new text is no longer present
                # the skill must have changed since promotion — abort safely.
                if candidate.proposed_patch_new and candidate.proposed_patch_old is not None:
                    err = self._skill_store.patch_skill(
                        skill_name,
                        candidate.proposed_patch_new,
                        candidate.proposed_patch_old,
                    )
                    if err:
                        return False, f"inverse patch failed: {err}"
                else:
                    return False, "no patch payload recorded; cannot invert"
            elif candidate.operation == "disable":
                disabled = getattr(self._skill_store, "_disabled", None)
                if disabled is not None and skill_name in disabled:
                    disabled.discard(skill_name)
            else:
                return False, f"unknown operation '{candidate.operation}'"
        except Exception as e:
            return False, f"rollback raised: {e}"

        candidate.status = "rolled_back"
        await self._store.update_candidate(candidate)
        logger.info("Rolled back skill '{}' (candidate id={})", skill_name, candidate.id)
        return True, f"skill '{skill_name}' rolled back"

    async def status_summary(self) -> dict[str, Any]:
        latest_run = await self._store.latest_run()
        unconsumed = await self._store.count_unconsumed()
        pending = await self._store.list_candidates(status="pending", limit=200)
        needs_review = await self._store.list_candidates(status="needs_review", limit=200)
        promoted = await self._store.list_candidates(status="promoted", limit=200)
        return {
            "enabled": self._config.enabled,
            "trigger_mode": self._config.trigger_mode,
            "scheduler_active": self._scheduler.mode != "manual",
            "trajectories_unconsumed": unconsumed,
            "candidates_pending": len(pending),
            "candidates_needs_review": len(needs_review),
            "candidates_promoted_total": len(promoted),
            "latest_run": latest_run.to_dict() if latest_run else None,
            "cooldowns": [
                {"skill": cd.skill_name, "until": datetime.fromtimestamp(cd.until_ts).isoformat()}
                for cd in self._cooldowns.values()
            ],
        }

    # ── Run (locked) ─────────────────────────────────────────────────────────

    async def _run_locked(self, *, trigger: EvolutionTrigger) -> EvolutionRun:
        run = EvolutionRun(triggered_by=trigger)
        started = time.monotonic()
        await self._store.save_run(run)

        try:
            trajectories = await self._collect_trajectories()
            run.trajectories_consumed = len(trajectories)

            if not trajectories:
                run.notes = "no trajectories available"
                await self._finalize_run(run, started)
                return run

            decision = await self._evolver.propose(trajectories, run_id=run.id)
            run.candidates_generated = len(decision.candidates)

            for candidate in decision.candidates:
                if self._is_in_cooldown(candidate.skill_name):
                    candidate.status = "rejected"
                    candidate.rejected_reason = "cooldown active for this skill"
                    await self._store.save_candidate(candidate)
                    run.candidates_rejected += 1
                    continue

                # Persist as pending before evaluation so the audit trail
                # captures the proposal even if eval crashes mid-run.
                candidate.status = "pending"
                await self._store.save_candidate(candidate)

                if not self._config.auto_promote:
                    candidate.status = "needs_review"
                    candidate.rejected_reason = "auto_promote disabled"
                    await self._store.update_candidate(candidate)
                    run.candidates_needs_review += 1
                    continue

                try:
                    await self._gate.evaluate(candidate)
                except Exception as e:
                    candidate.status = "rejected"
                    candidate.rejected_reason = f"gate raised: {e}"
                    await self._store.update_candidate(candidate)
                    run.candidates_rejected += 1
                    continue

                refreshed = await self._store.get_candidate(candidate.id)
                if refreshed is not None:
                    candidate = refreshed

                if candidate.status == "promoted":
                    run.candidates_promoted += 1
                    self._activate_cooldown(candidate.skill_name)
                elif candidate.status == "needs_review":
                    run.candidates_needs_review += 1
                else:
                    run.candidates_rejected += 1

            await self._store.mark_consumed(decision.consumed_trajectory_ids, run.id)
        except Exception as e:
            run.error = f"{type(e).__name__}: {e}"
            logger.error("EvolutionEngine run failed: {}", e)
        finally:
            await self._finalize_run(run, started)
        return run

    async def _finalize_run(self, run: EvolutionRun, started_monotonic: float) -> None:
        run.duration_ms = (time.monotonic() - started_monotonic) * 1000.0
        run.finished_at = datetime.now().isoformat()
        try:
            await self._store.save_run(run)
        except Exception as e:
            logger.warning("Failed to persist run {}: {}", run.id, e)

    async def _collect_trajectories(self) -> list[Trajectory]:
        trajectories = await self._store.list_trajectories(
            limit=self._config.max_trajectories_per_run,
            only_unconsumed=True,
        )
        # Heuristic prioritisation: failures first, then low-score successes,
        # then everything else, capped at max_trajectories_per_run.
        def sort_key(t: Trajectory) -> tuple[int, float]:
            outcome_rank = {"failure": 0, "partial": 1, "success": 2}.get(t.outcome, 3)
            score = t.reflection_score if t.reflection_score is not None else 1.0
            return (outcome_rank, score)

        trajectories.sort(key=sort_key)
        return trajectories[: self._config.max_trajectories_per_run]

    # ── Cooldowns ────────────────────────────────────────────────────────────

    def _activate_cooldown(self, skill_name: str) -> None:
        seconds = max(0, int(self._config.cooldown_seconds_after_promote))
        if seconds <= 0 or not skill_name:
            return
        self._cooldowns[skill_name] = _Cooldown(
            skill_name=skill_name,
            until_ts=time.time() + seconds,
        )

    def _is_in_cooldown(self, skill_name: str) -> bool:
        cd = self._cooldowns.get(skill_name)
        if cd is None:
            return False
        if time.time() >= cd.until_ts:
            self._cooldowns.pop(skill_name, None)
            return False
        return True
