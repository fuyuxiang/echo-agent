"""Risk classification for tool operations — determines approval requirements."""

from __future__ import annotations

from enum import Enum
from typing import Any


class RiskLevel(str, Enum):
    READ_ONLY = "read_only"
    WRITE = "write"
    EXEC = "exec"
    DANGEROUS = "dangerous"


_TOOL_RISK_MAP: dict[str, RiskLevel] = {
    # READ_ONLY — never needs approval
    "read_file": RiskLevel.READ_ONLY,
    "list_dir": RiskLevel.READ_ONLY,
    "search_files": RiskLevel.READ_ONLY,
    "knowledge_search": RiskLevel.READ_ONLY,
    "session_search": RiskLevel.READ_ONLY,
    "skills_list": RiskLevel.READ_ONLY,
    "skill_view": RiskLevel.READ_ONLY,
    "agents_list": RiskLevel.READ_ONLY,
    "agents_route": RiskLevel.READ_ONLY,
    "web_fetch": RiskLevel.READ_ONLY,
    "web_search": RiskLevel.READ_ONLY,
    "vision_analyze": RiskLevel.READ_ONLY,
    # WRITE — auto-approved on trusted channels
    "write_file": RiskLevel.WRITE,
    "edit_file": RiskLevel.WRITE,
    "patch": RiskLevel.WRITE,
    "knowledge_index": RiskLevel.WRITE,
    "todo": RiskLevel.WRITE,
    "task": RiskLevel.WRITE,
    "workflow": RiskLevel.WRITE,
    "notify": RiskLevel.WRITE,
    "message": RiskLevel.WRITE,
    "clarify": RiskLevel.WRITE,
    "memory": RiskLevel.WRITE,
    "image_generate": RiskLevel.WRITE,
    "text_to_speech": RiskLevel.WRITE,
    # EXEC — needs allowlist or smart approval
    "exec": RiskLevel.EXEC,
    "execute_code": RiskLevel.EXEC,
    "process": RiskLevel.EXEC,
    # delegate/spawn hand a goal to a worker agent that then calls tools of its
    # own — including exec. Classifying the dispatch itself as WRITE made the
    # pair a cheaper route to EXEC than exec: a caller who may not run commands
    # directly could ask a worker to run them. The worker's own calls are gated
    # too, but the dispatch is where the caller's authority is still known, so
    # it must not be the weaker of the two. Both tools also declare
    # risk_level="exec"; keeping the map in agreement is enforced by
    # tests/test_risk_classifier_consistency.py.
    "delegate_task": RiskLevel.EXEC,
    "spawn_task": RiskLevel.EXEC,
    # DANGEROUS — always needs manual approval
    "cronjob": RiskLevel.DANGEROUS,
    "skill_install": RiskLevel.DANGEROUS,
    "skill_manage": RiskLevel.DANGEROUS,
}


_SEVERITY: dict[RiskLevel, int] = {
    RiskLevel.READ_ONLY: 0,
    RiskLevel.WRITE: 1,
    RiskLevel.EXEC: 2,
    RiskLevel.DANGEROUS: 3,
}


def classify_risk(tool_name: str, arguments: dict[str, Any] | None = None, *, tool_risk_level: str = "") -> RiskLevel:
    """Classify the risk level of a tool call — the STRICTER of the two sources.

    The static map and a tool's declared ``risk_level`` describe the same fact,
    so whenever they disagree one of them is stale. Letting the map win
    unconditionally (the previous rule) meant a tool could declare
    ``risk_level="exec"`` and still be gated as WRITE forever, with nothing to
    surface the contradiction — that is how delegate_task/spawn_task became a
    cheaper path to EXEC than exec itself, despite both declaring "exec".

    Taking the maximum severity makes a stale entry fail safe rather than fail
    open: it can over-gate a call (visible, someone complains) but can no longer
    silently under-gate one. A tool that genuinely needs to be *less* restricted
    than its declaration has to say so by changing the declaration.

    Falls back to WRITE when neither source knows the tool.
    """
    static = _TOOL_RISK_MAP.get(tool_name)

    declared: RiskLevel | None = None
    if tool_risk_level:
        try:
            declared = RiskLevel(tool_risk_level)
        except ValueError:
            declared = None

    if static is None and declared is None:
        return RiskLevel.WRITE
    if static is None:
        return declared  # type: ignore[return-value]
    if declared is None:
        return static
    return max(static, declared, key=lambda level: _SEVERITY[level])
