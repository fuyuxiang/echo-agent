"""TrajectoryRecorder — collects runtime experience via plugin hooks.

The recorder maintains a per-session "active trajectory" that captures every
tool call and LLM iteration during a single user turn. ``AgentLoop`` calls
:meth:`begin_turn` at the start of ``_process_event`` and :meth:`end_turn`
when the turn finalizes; in between, plugin-style hooks (``post_tool_call``,
``post_llm_call``) feed structured records into the active trajectory.

The recorder never blocks the main loop: every public method is short and
exception-safe, with persistence delegated to :class:`TrajectoryStore`.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from loguru import logger

from echo_agent.evolution.types import (
    Outcome,
    ToolCall,
    Trajectory,
    digest,
)

if TYPE_CHECKING:
    from echo_agent.agent.planning.reflection import ReflectionModule
    from echo_agent.evolution.store import TrajectoryStore
    from echo_agent.plugins.hooks import HookRegistry
    from echo_agent.skills.store import SkillStore


@dataclass
class _Partial:
    session_id: str = ""
    chat_id: str = ""
    channel: str = ""
    task_input: str = ""
    started_at: float = 0.0
    iterations: int = 0
    tools_called: list[ToolCall] = field(default_factory=list)
    last_response_content: str = ""
    model_used: str = ""
    skills_active: list[str] = field(default_factory=list)


class TrajectoryRecorder:
    """Captures trajectories during agent turns and persists them on completion."""

    def __init__(
        self,
        store: TrajectoryStore,
        *,
        redact_args: bool = True,
        skill_store: SkillStore | None = None,
        reflection: ReflectionModule | None = None,
        max_input_chars: int = 2_000,
        max_response_chars: int = 4_000,
    ):
        self._store = store
        self._redact = redact_args
        self._skill_store = skill_store
        self._reflection = reflection
        self._active: dict[str, _Partial] = {}
        self._lock = asyncio.Lock()
        self._max_input_chars = max_input_chars
        self._max_response_chars = max_response_chars

    # ── Hook attachment ──────────────────────────────────────────────────────

    def attach(self, hooks: HookRegistry) -> None:
        """Subscribe hook callbacks. Idempotent — re-attaching is safe."""
        # Unregister any previous evolution hooks so re-attaching during tests
        # does not stack duplicate handlers on the registry.
        hooks.unregister_plugin("evolution")
        hooks.register("post_tool_call", self._on_post_tool_call, plugin="evolution")
        hooks.register("post_llm_call", self._on_post_llm_call, plugin="evolution")
        hooks.register("on_error", self._on_error, plugin="evolution")

    def detach(self, hooks: HookRegistry) -> None:
        hooks.unregister_plugin("evolution")

    # ── Turn boundaries (called from AgentLoop) ─────────────────────────────

    async def begin_turn(
        self,
        *,
        session_key: str,
        chat_id: str,
        channel: str,
        task_input: str,
        model_used: str = "",
    ) -> None:
        if not session_key:
            return
        snapshot = self._snapshot_active_skills()
        async with self._lock:
            self._active[session_key] = _Partial(
                session_id=session_key,
                chat_id=chat_id,
                channel=channel,
                task_input=task_input[: self._max_input_chars],
                started_at=time.monotonic(),
                model_used=model_used,
                skills_active=snapshot,
            )

    async def end_turn(
        self,
        *,
        session_key: str,
        response_text: str = "",
        error: str = "",
        iteration_count: int | None = None,
        outcome: Outcome | None = None,
        task_type: str = "",
        spawn_fn: Any = None,
    ) -> Trajectory | None:
        """Finish the turn. The pop + trajectory build are synchronous (the
        next ``begin_turn`` for this session must not race them); the slow
        part — LLM reflection + persistence — runs in the background when a
        ``spawn_fn`` is provided, so it never extends the session-lock window.
        """
        async with self._lock:
            partial = self._active.pop(session_key, None)
        if partial is None:
            return None

        duration_ms = max(0.0, (time.monotonic() - partial.started_at) * 1000.0)
        if iteration_count is not None:
            partial.iterations = max(partial.iterations, int(iteration_count))

        derived_outcome: Outcome
        if outcome is not None:
            derived_outcome = outcome
        elif error:
            derived_outcome = "failure"
        else:
            derived_outcome = self._infer_outcome(partial, response_text)

        trajectory = Trajectory(
            session_id=partial.session_id,
            chat_id=partial.chat_id,
            channel=partial.channel,
            task_input=partial.task_input,
            task_type=task_type,
            tools_called=list(partial.tools_called),
            iterations=partial.iterations,
            duration_ms=duration_ms,
            final_response=(response_text or partial.last_response_content)[: self._max_response_chars],
            failure_reason=error[:1000] if error else "",
            outcome=derived_outcome,
            skills_active=list(partial.skills_active),
            model_used=partial.model_used,
        )

        if spawn_fn is not None:
            # Reflection is an LLM call and persistence hits the DB — neither
            # needs to delay the caller (which may hold the session lock).
            spawn_fn(self._reflect_and_store(trajectory, partial, error, response_text))
            return trajectory

        persisted = await self._reflect_and_store(trajectory, partial, error, response_text)
        return trajectory if persisted else None

    async def _reflect_and_store(
        self,
        trajectory: Trajectory,
        partial: _Partial,
        error: str,
        response_text: str,
    ) -> bool:
        if self._reflection is not None and not error and partial.iterations > 0:
            try:
                feedback = await self._safe_reflect(trajectory)
                if feedback is not None:
                    trajectory.reflection_score = float(feedback.get("score", 0.5))
                    trajectory.reflection_critique = str(feedback.get("critique", ""))[:1000]
                    trajectory.reflection_suggestions = [
                        str(s)[:300]
                        for s in (feedback.get("suggestions") or [])
                        if s
                    ][:5]
            except Exception as e:
                logger.debug("Reflection during trajectory finalize failed: {}", e)

        try:
            await self._store.append_trajectory(trajectory)
            return True
        except Exception as e:
            logger.warning("Failed to persist trajectory: {}", e)
            return False

    async def discard_turn(self, session_key: str) -> None:
        async with self._lock:
            self._active.pop(session_key, None)

    async def has_active(self, session_key: str) -> bool:
        async with self._lock:
            return session_key in self._active

    # ── Hook callbacks ──────────────────────────────────────────────────────

    async def _on_post_tool_call(self, result: Any, name: str, args: Any, ctx: Any):
        try:
            session_key = getattr(ctx, "session_key", "") if ctx is not None else ""
            if not session_key:
                return None
            async with self._lock:
                partial = self._active.get(session_key)
            if partial is None:
                return None
            success = bool(getattr(result, "success", True))
            error = str(getattr(result, "error", "") or "")
            output = str(getattr(result, "text", "") or getattr(result, "output", "") or "")
            tc = ToolCall(
                name=str(name or ""),
                args_digest=digest(args) if self._redact else (str(args)[:500]),
                result_digest=digest(output) if self._redact else output[:500],
                duration_ms=0.0,
                success=success,
                error=error[:300],
            )
            async with self._lock:
                live = self._active.get(session_key)
                if live is partial:
                    live.tools_called.append(tc)
        except Exception as e:
            logger.debug("Recorder post_tool_call failed: {}", e)
        return None

    async def _on_post_llm_call(self, response: Any, *_args: Any, **_kwargs: Any):
        try:
            # No ctx is provided to post_llm_call by InferenceStage today, so we
            # bump iteration counters for every active turn. Tool-call success
            # remains accurately attributed via post_tool_call's ctx.session_key.
            content = str(getattr(response, "content", "") or "")
            async with self._lock:
                for partial in self._active.values():
                    partial.iterations += 1
                    if content:
                        partial.last_response_content = content[: self._max_response_chars]
        except Exception as e:
            logger.debug("Recorder post_llm_call failed: {}", e)
        return None

    async def _on_error(self, *args: Any, **kwargs: Any):
        # Errors are surfaced through end_turn(error=...) by AgentLoop; this is
        # only a defensive log so plugin hook errors do not propagate.
        try:
            logger.debug("Recorder on_error hook: args={} kwargs={}", args, kwargs)
        except Exception:
            pass
        return None

    # ── Internals ───────────────────────────────────────────────────────────

    def _snapshot_active_skills(self) -> list[str]:
        if self._skill_store is None:
            return []
        try:
            return [m.name for m in self._skill_store.list_all()]
        except Exception:
            return []

    def _infer_outcome(self, partial: _Partial, response_text: str) -> Outcome:
        if partial.tools_called:
            failed = sum(1 for t in partial.tools_called if not t.success)
            if failed and failed >= max(1, len(partial.tools_called) // 2):
                return "partial"
        if not response_text and not partial.last_response_content:
            return "partial"
        return "success"

    async def _safe_reflect(self, trajectory: Trajectory) -> dict[str, Any] | None:
        """Run the existing ReflectionModule on the captured trajectory."""
        if self._reflection is None:
            return None
        # ReflectionModule.critique expects (Plan, list[str]); we adapt by
        # constructing a one-step pseudo-plan whose results array contains the
        # final response. This re-uses the exact same critique tool the planner
        # already validates, instead of inventing a parallel format.
        try:
            from echo_agent.agent.planning.models import Plan, PlanStep, StrategyType
        except Exception:
            return None
        try:
            step = PlanStep(index=0, description=trajectory.task_input or "(unknown task)")
            plan = Plan(
                strategy=StrategyType.REACT,
                goal=trajectory.task_input or "task",
                steps=[step],
            )
        except Exception:
            return None
        try:
            results = [trajectory.final_response or "(no response)"]
            feedback = await self._reflection.critique(plan, results)
            return {
                "score": getattr(feedback, "score", 0.5),
                "critique": getattr(feedback, "critique", ""),
                "suggestions": list(getattr(feedback, "suggestions", []) or []),
            }
        except Exception as e:
            logger.debug("Reflection critique raised: {}", e)
            return None
