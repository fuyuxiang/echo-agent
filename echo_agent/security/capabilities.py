"""Tool capability classification used by policy and audit code."""

from __future__ import annotations

from typing import Any


TOOL_CAPABILITIES: dict[str, frozenset[str]] = {
    "agents_list": frozenset({"agent.read"}),
    "agents_route": frozenset({"agent.dispatch"}),
    "clarify": frozenset({"message.ask"}),
    "cronjob": frozenset({"scheduler.write"}),
    "edit_file": frozenset({"fs.read", "fs.write"}),
    "exec": frozenset({"process.exec"}),
    "execute_code": frozenset({"code.exec", "process.exec"}),
    "image_generate": frozenset({"media.generate", "network.outbound"}),
    "knowledge_index": frozenset({"knowledge.write", "fs.read"}),
    "knowledge_search": frozenset({"knowledge.read"}),
    "list_dir": frozenset({"fs.read"}),
    "memory": frozenset({"memory.read", "memory.write"}),
    # The MCP resource/prompt tools reach a third-party server just as an
    # mcp_* tool call does, so they carry the same capability and are covered by
    # the same deny lists. Named here as well as declared on the classes: the
    # name-prefix fallback in tool_capabilities() would give them mcp.call
    # anyway, but a reader greps this table.
    "mcp_prompts": frozenset({"mcp.call"}),
    "mcp_resources": frozenset({"mcp.call"}),
    "message": frozenset({"message.send"}),
    "notify": frozenset({"message.send"}),
    "patch": frozenset({"fs.read", "fs.write"}),
    "process": frozenset({"process.exec", "process.manage"}),
    "read_file": frozenset({"fs.read"}),
    "read_spill": frozenset({"fs.read"}),
    "search_files": frozenset({"fs.read"}),
    "session_search": frozenset({"session.read"}),
    "skill_install": frozenset({"skill.install", "network.outbound", "fs.write"}),
    "skill_manage": frozenset({"skill.write", "fs.write"}),
    # skill_run spawns a Python interpreter on a file the skill author controls,
    # so it is code execution by any measure — an empty capability set let it
    # slip past every deny list built out of capabilities (DAEMON_*,
    # PUBLIC_GATEWAY_*), which is exactly how it became a cheaper route to
    # process.exec than exec itself. code.exec belongs here as much as
    # process.exec: what it runs is Python source, same as execute_code.
    "skill_run": frozenset({"process.exec", "code.exec", "skill.read"}),
    "skill_view": frozenset({"skill.read"}),
    "skills_list": frozenset({"skill.read"}),
    "task": frozenset({"task.write"}),
    "text_to_speech": frozenset({"media.generate", "network.outbound"}),
    "todo": frozenset({"task.write"}),
    "vision_analyze": frozenset({"media.read"}),
    "web_fetch": frozenset({"network.outbound"}),
    "web_search": frozenset({"network.outbound"}),
    "workflow": frozenset({"workflow.write"}),
    "write_file": frozenset({"fs.write"}),
}


def tool_name(tool_or_name: Any) -> str:
    return tool_or_name if isinstance(tool_or_name, str) else getattr(tool_or_name, "name", "")


def tool_capabilities(tool_or_name: Any) -> frozenset[str]:
    declared = getattr(tool_or_name, "capabilities", None)
    if declared:
        return frozenset(declared)
    name = tool_name(tool_or_name)
    if name.startswith("mcp_"):
        return frozenset({"mcp.call"})
    return TOOL_CAPABILITIES.get(name, frozenset())
