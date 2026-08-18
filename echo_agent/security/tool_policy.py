"""Tool exposure policy based on stable names plus coarse capabilities."""

from __future__ import annotations

from collections.abc import Iterable
from typing import TYPE_CHECKING

from loguru import logger

from echo_agent.security.capabilities import tool_capabilities, tool_name as resolve_tool_name

if TYPE_CHECKING:
    from echo_agent.tools.base import Tool
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
    # spill 对所有 profile 生效,取回工具就必须同样对所有 profile 可见。放在
    # MINIMAL 里而不是 CODING:minimal/messaging 恰是 exec 关掉的部署,那里
    # 没有 shell 兜底,取回工具被策略过滤掉就等于产物彻底不可达。
    "read_spill",
    "search_files",
    "session_search",
    "skill_view",
    "skills_list",
    "todo",
})

MESSAGING_TOOLS = MINIMAL_TOOLS | frozenset({
    "image_generate",
    "memory",
    "text_to_speech",
    "vision_analyze",
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
PUBLIC_GATEWAY_DENY_CAPABILITIES = frozenset({
    "code.exec",
    "fs.write",
    "process.exec",
    "process.manage",
    "scheduler.write",
    "skill.install",
    "skill.write",
    "workflow.write",
})

DAEMON_DENY_BY_DEFAULT = frozenset({
    "exec",
    "execute_code",
    "process",
    "skill_install",
})
DAEMON_DENY_CAPABILITIES = frozenset({
    "code.exec",
    "process.exec",
    "process.manage",
    "skill.install",
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


def is_tool_allowed(config: "Config", tool: object) -> bool:
    """Return whether a tool should be exposed to the model."""
    tools_cfg = config.tools
    explicit = _explicit_allow(config)
    deny = set(tools_cfg.deny or [])
    name = resolve_tool_name(tool)
    capabilities = tool_capabilities(tool)
    profile = tools_cfg.profile

    if name in deny:
        return False

    if tools_cfg.allow:
        base_allowed = name in tools_cfg.allow
    else:
        base_allowed = _profile_allows(profile, name) or name in tools_cfg.also_allow

    if not base_allowed:
        return False

    security_profile = config.security.profile
    if (
        security_profile == "public_gateway"
        and (name in PUBLIC_GATEWAY_DENY or capabilities.intersection(PUBLIC_GATEWAY_DENY_CAPABILITIES))
        and name not in explicit
    ):
        return False
    if (
        security_profile == "daemon"
        and (name in DAEMON_DENY_BY_DEFAULT or capabilities.intersection(DAEMON_DENY_CAPABILITIES))
        and name not in explicit
    ):
        return False

    if config.execution.network_policy == "deny" and (
        name in {"web_fetch", "web_search"} or "network.outbound" in capabilities
    ):
        return False

    return True


def filter_tools_by_policy(config: "Config", tools: Iterable["Tool"]) -> list["Tool"]:
    allowed: list[Tool] = []
    skipped: list[str] = []
    for tool in tools:
        if is_tool_allowed(config, tool):
            allowed.append(tool)
        else:
            skipped.append(tool.name)
    if skipped:
        logger.info("Tool policy skipped {} tools: {}", len(skipped), ", ".join(sorted(skipped)))
    return allowed
