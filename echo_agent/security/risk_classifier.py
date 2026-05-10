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
    "delegate_task": RiskLevel.WRITE,
    "spawn_task": RiskLevel.WRITE,
    # EXEC — needs allowlist or smart approval
    "exec": RiskLevel.EXEC,
    "execute_code": RiskLevel.EXEC,
    "process": RiskLevel.EXEC,
    # DANGEROUS — always needs manual approval
    "cronjob": RiskLevel.DANGEROUS,
    "skill_install": RiskLevel.DANGEROUS,
    "skill_manage": RiskLevel.DANGEROUS,
}


def classify_risk(tool_name: str, arguments: dict[str, Any] | None = None, *, tool_risk_level: str = "") -> RiskLevel:
    """Classify the risk level of a tool call.

    Priority:
    1. Static map lookup by tool name
    2. Tool's declared risk_level attribute
    3. Dynamic classification based on execution_mode
    4. Default: WRITE
    """
    if tool_name in _TOOL_RISK_MAP:
        return _TOOL_RISK_MAP[tool_name]

    if tool_risk_level:
        try:
            return RiskLevel(tool_risk_level)
        except ValueError:
            pass

    return RiskLevel.WRITE
