"""Evolver — LLM-driven proposal of new / patched skills from trajectories.

Given a batch of recent trajectories, the evolver groups them by signal
(failures, low-score successes, recurring untyped tasks) and asks an LLM to
emit candidate skill operations through a structured tool-call interface.
Every candidate must include a falsifiable expected_improvement so the
PromotionGate can later check whether the change paid off.

The evolver does NOT touch the on-disk skill store directly; it only writes
:class:`SkillCandidate` records. The :class:`PromotionGate` is responsible
for the actual A/B test and promotion.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from loguru import logger

from echo_agent.evolution.types import (
    SkillCandidate,
    Trajectory,
    digest,
)

if TYPE_CHECKING:
    from echo_agent.evolution.store import TrajectoryStore
    from echo_agent.models.provider import LLMProvider
    from echo_agent.skills.store import SkillStore


_SYSTEM_PROMPT = """\
You are the evolver of a self-improving agent's skill library.

You read past trajectories (task → tools used → outcome → reflection) and
propose changes to the agent's skill library so that future tasks of the
same kind succeed more reliably or with fewer steps.

You can ONLY make these three operations:
  - create:  add a brand-new skill (full SKILL.md content)
  - patch:   replace a substring inside an existing skill's SKILL.md
  - disable: mark a harmful or obsolete skill as disabled

Hard rules:
  1. Every proposal MUST include a `rationale` (why this change addresses the
     observed problem) and an `expected_improvement` describing a concrete,
     falsifiable prediction such as
     "tasks of type 'web summarize' will go from N% to M% pass rate".
  2. SKILL.md content MUST start with YAML frontmatter containing at least
     `name` and `description`. Keep skills concise (under ~6kB) and procedural
     ("when X, do Y, then verify Z, fall back to W").
  3. Skills must NEVER touch sensitive paths, exfiltrate data, disable
     security checks, or override approval flows.
  4. Skill names use lowercase letters, digits, hyphens, underscores. Max 64
     characters. New skills must not collide with existing names unless you
     are patching them via the `patch` operation.
  5. Be conservative. Prefer ONE high-leverage change over many tiny edits.
     Do not propose a change unless the trajectories clearly justify it.

Submit each proposal by calling the `propose_skill` tool. When you have
nothing useful to propose, reply with the literal text "No changes needed."
and stop.
"""


@dataclass
class EvolverDecision:
    candidates: list[SkillCandidate]
    consumed_trajectory_ids: list[str]


_PROPOSE_TOOL = [
    {
        "type": "function",
        "function": {
            "name": "propose_skill",
            "description": (
                "Propose a single skill-library change. Call once per change."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["create", "patch", "disable"],
                    },
                    "skill_name": {
                        "type": "string",
                        "description": (
                            "Lowercase, hyphen-separated. Must match the existing skill"
                            " for patch/disable; must be new for create."
                        ),
                    },
                    "content": {
                        "type": "string",
                        "description": (
                            "Full SKILL.md content with YAML frontmatter. Required for"
                            " create. Must include 'name' and 'description' fields."
                        ),
                    },
                    "patch_old": {
                        "type": "string",
                        "description": "Required for patch: exact substring to replace.",
                    },
                    "patch_new": {
                        "type": "string",
                        "description": "Required for patch: replacement substring.",
                    },
                    "rationale": {
                        "type": "string",
                        "description": "Why this change addresses observed trajectory failures.",
                    },
                    "expected_improvement": {
                        "type": "string",
                        "description": (
                            "Concrete falsifiable prediction the PromotionGate can verify"
                            " against the baseline eval dataset."
                        ),
                    },
                    "source_trajectory_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Trajectory IDs from the briefing that motivated this proposal.",
                    },
                },
                "required": ["operation", "skill_name", "rationale", "expected_improvement"],
            },
        },
    }
]


class Evolver:
    """Generates skill candidates from a batch of trajectories."""

    _NAME_MAX = 64

    def __init__(
        self,
        provider: LLMProvider,
        store: TrajectoryStore,
        skill_store: SkillStore,
        *,
        model: str = "",
        max_candidates: int = 3,
        max_iterations: int = 10,
        skill_size_limit_bytes: int = 50_000,
        forbidden_name_prefixes: tuple[str, ...] = ("evolution", "system", "internal"),
        router: object | None = None,
    ):
        self._provider = provider
        self._store = store
        self._skill_store = skill_store
        self._model = model
        self._router = router
        self._max_candidates = max_candidates
        self._max_iterations = max_iterations
        self._skill_size_limit = skill_size_limit_bytes
        self._forbidden_prefixes = tuple(p.lower() for p in forbidden_name_prefixes)

    async def propose(
        self,
        trajectories: list[Trajectory],
        *,
        run_id: str,
    ) -> EvolverDecision:
        if not trajectories:
            return EvolverDecision(candidates=[], consumed_trajectory_ids=[])

        briefing = self._build_briefing(trajectories)
        existing_skills = self._format_existing_skills()

        messages: list[dict[str, Any]] = [
            {"role": "system", "content": _SYSTEM_PROMPT},
            {
                "role": "user",
                "content": (
                    "## Existing skills\n"
                    f"{existing_skills or '(none)'}\n\n"
                    "## Recent trajectories\n"
                    f"{briefing}\n\n"
                    f"You may issue at most {self._max_candidates} `propose_skill`"
                    " tool calls. Stop earlier if no further changes are warranted."
                ),
            },
        ]

        candidates: list[SkillCandidate] = []
        consumed: set[str] = set()

        for _ in range(self._max_iterations):
            try:
                call_provider, call_model = self._provider, self._model
                if self._router is not None and hasattr(self._router, "resolve"):
                    routed_provider, routed_model = self._router.resolve(
                        "evolution",
                        fallback_provider=self._provider,
                        fallback_model=self._model,
                    )
                    if routed_provider is not None:
                        call_provider, call_model = routed_provider, routed_model
                response = await call_provider.chat_with_retry(
                    messages=messages,
                    tools=_PROPOSE_TOOL,
                    model=call_model or None,
                    max_tokens=2048,
                    temperature=0.4,
                )
            except Exception as e:
                logger.warning("Evolver LLM call failed: {}", e)
                break

            assistant_msg: dict[str, Any] = {
                "role": "assistant",
                "content": response.content or "",
            }
            if response.tool_calls:
                assistant_msg["tool_calls"] = [tc.to_openai_format() for tc in response.tool_calls]
            messages.append(assistant_msg)

            if not response.has_tool_calls:
                break

            for tc in response.tool_calls:
                if tc.name != "propose_skill":
                    messages.append({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "name": tc.name,
                        "content": f"Error: unknown tool '{tc.name}'",
                    })
                    continue
                args = tc.arguments
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except Exception:
                        args = {}
                candidate, error = self._build_candidate(
                    args=args or {},
                    trajectories=trajectories,
                    run_id=run_id,
                )
                if candidate is None:
                    messages.append({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "name": tc.name,
                        "content": f"Error: {error or 'invalid proposal'}",
                    })
                    continue
                candidates.append(candidate)
                consumed.update(candidate.source_trajectories)
                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "name": tc.name,
                    "content": (
                        f"Accepted candidate '{candidate.skill_name}' "
                        f"({candidate.operation}). id={candidate.id}"
                    ),
                })
                if len(candidates) >= self._max_candidates:
                    break

            if len(candidates) >= self._max_candidates:
                break

        return EvolverDecision(
            candidates=candidates,
            consumed_trajectory_ids=sorted(consumed),
        )

    # ── Validation ──────────────────────────────────────────────────────────

    def _build_candidate(
        self,
        *,
        args: dict[str, Any],
        trajectories: list[Trajectory],
        run_id: str,
    ) -> tuple[SkillCandidate | None, str]:
        operation = (args.get("operation") or "").strip().lower()
        if operation not in {"create", "patch", "disable"}:
            return None, "operation must be one of create/patch/disable"

        skill_name = (args.get("skill_name") or "").strip().lower()
        if not skill_name:
            return None, "skill_name is required"
        if not all(ch.isalnum() or ch in "._-" for ch in skill_name) or len(skill_name) > self._NAME_MAX:
            return None, "invalid skill_name (lowercase alphanumeric, ._- only, max 64 chars)"
        for prefix in self._forbidden_prefixes:
            if skill_name.startswith(prefix):
                return None, f"skill_name may not start with reserved prefix '{prefix}'"

        rationale = (args.get("rationale") or "").strip()
        expected_improvement = (args.get("expected_improvement") or "").strip()
        if not rationale or not expected_improvement:
            return None, "rationale and expected_improvement are required"

        source_ids_raw = args.get("source_trajectory_ids") or []
        valid_ids = {t.id for t in trajectories}
        source_ids = [s for s in source_ids_raw if isinstance(s, str) and s in valid_ids]
        if not source_ids:
            # Fall back to attributing all briefing trajectories — keeps the
            # audit trail intact when the model forgets to name them.
            source_ids = [t.id for t in trajectories[:5]]

        existing_meta = {m.name for m in self._safe_list_skills()}

        if operation == "create":
            content = args.get("content") or ""
            if not content.strip():
                return None, "content is required for create"
            if len(content.encode("utf-8")) > self._skill_size_limit:
                return None, f"content exceeds {self._skill_size_limit} byte limit"
            if not content.lstrip().startswith("---"):
                return None, "SKILL.md content must begin with YAML frontmatter"
            if "description:" not in content:
                return None, "SKILL.md frontmatter must include a description field"
            if skill_name in existing_meta:
                return None, f"skill '{skill_name}' already exists; use patch instead"

            return (
                SkillCandidate(
                    operation="create",
                    skill_name=skill_name,
                    proposed_content=content,
                    rationale=rationale,
                    expected_improvement=expected_improvement,
                    source_trajectories=source_ids,
                    run_id=run_id,
                ),
                "",
            )

        if operation == "patch":
            patch_old = args.get("patch_old") or ""
            patch_new = args.get("patch_new") or ""
            if not patch_old:
                return None, "patch_old is required for patch"
            if skill_name not in existing_meta:
                return None, f"cannot patch unknown skill '{skill_name}'"
            return (
                SkillCandidate(
                    operation="patch",
                    skill_name=skill_name,
                    parent_skill=skill_name,
                    proposed_patch_old=patch_old,
                    proposed_patch_new=patch_new,
                    rationale=rationale,
                    expected_improvement=expected_improvement,
                    source_trajectories=source_ids,
                    run_id=run_id,
                ),
                "",
            )

        # operation == "disable"
        if skill_name not in existing_meta:
            return None, f"cannot disable unknown skill '{skill_name}'"
        return (
            SkillCandidate(
                operation="disable",
                skill_name=skill_name,
                parent_skill=skill_name,
                rationale=rationale,
                expected_improvement=expected_improvement,
                source_trajectories=source_ids,
                run_id=run_id,
            ),
            "",
        )

    # ── Briefing ────────────────────────────────────────────────────────────

    def _build_briefing(self, trajectories: list[Trajectory]) -> str:
        lines: list[str] = []
        for t in trajectories:
            tools_used = ", ".join(sorted({tc.name for tc in t.tools_called})) or "(no tools)"
            score = "n/a" if t.reflection_score is None else f"{t.reflection_score:.2f}"
            failure_note = f" failure='{t.failure_reason[:120]}'" if t.failure_reason else ""
            suggestions = "; ".join(t.reflection_suggestions[:3])
            suggestion_note = f" suggestions='{suggestions}'" if suggestions else ""
            input_excerpt = digest(t.task_input, max_chars=160)
            response_excerpt = digest(t.final_response, max_chars=160)
            lines.append(
                f"- id={t.id} outcome={t.outcome} score={score} "
                f"task_type='{t.task_type or 'unknown'}' iterations={t.iterations} "
                f"tools=[{tools_used}]{failure_note}{suggestion_note}\n"
                f"    input: {input_excerpt}\n"
                f"    response: {response_excerpt}"
            )
        return "\n".join(lines) if lines else "(no trajectories provided)"

    def _format_existing_skills(self) -> str:
        metas = self._safe_list_skills()
        if not metas:
            return ""
        return "\n".join(f"- {m.name}: {m.description}" for m in metas[:30])

    def _safe_list_skills(self) -> list[Any]:
        try:
            return list(self._skill_store.list_all())
        except Exception:
            return []
