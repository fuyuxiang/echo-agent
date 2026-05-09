"""Tool exposure policy.

The policy is intentionally name-based because Echo Agent's tools already have
stable public names and multi-agent profiles also refer to tools by name.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import TYPE_CHECKING

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.agent.tools.base import Tool
    from echo_agent.config.schema import Config


MINIMAL_TOOLS = frozenset({
    "agents_list",
    "agents_route",
    "clarify",
    "knowledge_search",
    "list_dir",
    "message",
    "notify",
    "read_file",
    "search_files",
    "session_search",
    "skill_view",
    "skills_list",
    "todo",
})

MESSAGING_TOOLS = MINIMAL_TOOLS | frozenset({
    "memory",
    "tts",
    "vision",
})

CODING_TOOLS = MESSAGING_TOOLS | frozenset({
    "edit_file",
    "knowledge_index",
    "patch",
    "task",
    "workflow",
    "write_file",
})

HIGH_RISK_TOOLS = frozenset({
    "cronjob",
    "exec",
    "execute_code",
    "process",
    "skill_install",
    "skill_manage",
})

PUBLIC_GATEWAY_DENY = HIGH_RISK_TOOLS | frozenset({
    "edit_file",
    "knowledge_index",
    "patch",
    "workflow",
    "write_file",
})

DAEMON_DENY_BY_DEFAULT = frozenset({
    "exec",
    "execute_code",
    "process",
    "skill_install",
})

PROFILE_TOOLS = {
    "minimal": MINIMAL_TOOLS,
    "messaging": MESSAGING_TOOLS,
    "coding": CODING_TOOLS,
    "full": frozenset({"*"}),
}


def _profile_allows(profile: str, tool_name: str) -> bool:
    allowed = PROFILE_TOOLS.get(profile, CODING_TOOLS)
    return "*" in allowed or tool_name in allowed


def _explicit_allow(config: "Config") -> set[str]:
    tools_cfg = config.tools
    return set(tools_cfg.allow or []) | set(tools_cfg.also_allow or [])


def is_tool_allowed(config: "Config", tool_name: str) -> bool:
    """Return whether a tool should be exposed to the model."""
    tools_cfg = config.tools
    explicit = _explicit_allow(config)
    deny = set(tools_cfg.deny or [])
    profile = tools_cfg.profile

    if tool_name in deny:
        return False

    if tools_cfg.allow:
        base_allowed = tool_name in tools_cfg.allow
    else:
        base_allowed = _profile_allows(profile, tool_name) or tool_name in tools_cfg.also_allow

    if not base_allowed:
        return False

    security_profile = config.security.profile
    if security_profile == "public_gateway" and tool_name in PUBLIC_GATEWAY_DENY and tool_name not in explicit:
        return False
    if security_profile == "daemon" and tool_name in DAEMON_DENY_BY_DEFAULT and tool_name not in explicit:
        return False

    if config.execution.network_policy == "deny" and tool_name in {"web_fetch", "web_search"}:
        return False

    return True


def filter_tools_by_policy(config: "Config", tools: Iterable["Tool"]) -> list["Tool"]:
    allowed: list[Tool] = []
    skipped: list[str] = []
    for tool in tools:
        if is_tool_allowed(config, tool.name):
            allowed.append(tool)
        else:
            skipped.append(tool.name)
    if skipped:
        logger.info("Tool policy skipped {} tools: {}", len(skipped), ", ".join(sorted(skipped)))
    return allowed
