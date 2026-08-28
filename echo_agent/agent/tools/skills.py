"""Agent-facing skill tools — skills_list, skill_view, skill_manage.

Provides the LLM with progressive-disclosure access to the skill store
and the ability to create/edit/delete skills for self-learning.
"""

from __future__ import annotations

import json
from typing import Any

from loguru import logger

from echo_agent.tools import Tool, ToolExecutionContext, ToolResult
from echo_agent.dependencies.lazy_deps import _is_satisfied
from echo_agent.skills.store import SkillStore


class SkillsListTool(Tool):
    name = "skills_list"
    risk_level = "read_only"
    description = (
        "List all available skills with compact metadata (name, description, category, version). "
        "Use this to discover what skills exist before viewing or managing them."
    )
    parameters = {"type": "object", "properties": {}, "required": []}

    def __init__(self, store: SkillStore):
        self._store = store

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        skills = self._store.list_all()
        if not skills:
            return ToolResult(success=True, output="No skills found.")
        data = [s.to_dict() for s in skills]
        return ToolResult(success=True, output=json.dumps(data, ensure_ascii=False, indent=2))


class SkillViewTool(Tool):
    name = "skill_view"
    risk_level = "read_only"
    # Pure reads now that dependency installs moved to skill_run, so the default
    # tool timeout is plenty. This used to be ~630s to accommodate an approval
    # wait plus a pip install inside a tool advertised as read_only.
    timeout_seconds = 60
    description = (
        "View the full content of a skill (SKILL.md) or a specific supporting file. "
        "Without file_path, returns the full SKILL.md and lists linked files. "
        "With file_path, returns that specific file from references/, templates/, scripts/, or assets/."
    )
    parameters = {
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Skill name to view"},
            "file_path": {
                "type": "string",
                "description": "Optional path to a supporting file (e.g. 'references/api.md')",
            },
        },
        "required": ["name"],
    }

    # approval/bus/config are still accepted so the call site in tools/__init__
    # (and any external construction) keeps working, but this tool no longer
    # installs anything, so it does not use them.
    def __init__(self, store: SkillStore, *, approval: Any = None, bus: Any = None, config: Any = None):
        self._store = store
        self._config = config

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"

    def _skill_pip_specs(self, name: str, content: str) -> list[str]:
        """Read a skill's declared pip deps from SKILL.md metadata.echo.requires.pip."""
        from echo_agent.skills.store import parse_frontmatter

        try:
            fm, _ = parse_frontmatter(content)
        except Exception:
            return []
        echo_meta = (fm.get("metadata", {}) or {}).get("echo", {}) or {}
        requires = echo_meta.get("requires", {}) or {}
        return list(requires.get("pip", []) or [])

    async def _precheck_deps(
        self, name: str, content: str, ctx: ToolExecutionContext | None
    ) -> str:
        """Report missing pip deps declared by the skill. Never installs.

        This used to install them — on a trusted CLI silently, elsewhere behind
        an approval prompt. Two problems with that. The tool declares
        ``risk_level="read_only"`` and is exposed in every profile including
        public_gateway, so "read the docs for this skill" was a reachable path to
        running pip; and ``install_authorized`` deliberately bypasses the
        SKILL_DEPS allowlist, ``skills.allow_lazy_installs`` and
        ``ECHO_AGENT_DISABLE_LAZY_INSTALLS``, so an externally authored SKILL.md
        naming a hostile package got it built (pip executes build code) merely by
        being looked at.

        Reporting keeps the part users actually wanted — knowing up front why a
        skill will not run — and leaves installing to skill_run, which is gated
        as EXEC and already has the approval closed-loop for it.
        """
        specs = self._skill_pip_specs(name, content)
        if not specs:
            return ""
        missing = [s for s in specs if not _is_satisfied(s)]
        if not missing:
            return ""

        spec_list = " ".join(missing)
        env_note = self._env_notice(content)
        return (
            f"\n\n---\n注意:技能「{name}」声明的依赖 {spec_list} 尚未安装,"
            "其脚本目前无法运行。使用 skill_run 运行该技能时会请求授权并安装这些依赖。"
            + env_note
        )

    def _env_notice(self, content: str) -> str:
        """Flag credential keys the skill declares but the environment lacks.

        Cheap to compute and it answers the question a user would otherwise hit
        as a mid-script failure ("why did image-gen exit immediately?").
        """
        try:
            from echo_agent.skills.env import declared_env_keys
            import os

            missing_env = [k for k in declared_env_keys(content) if not os.environ.get(k)]
        except Exception:
            return ""
        if not missing_env:
            return ""
        return (
            "\n此外,该技能需要以下环境变量,当前未设置:"
            + "、".join(missing_env)
        )

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        name = params["name"]
        file_path = params.get("file_path", "")

        # Disabled skills no longer resolve, so say so rather than "not found"
        # for a skill the user can plainly see in the admin UI.
        disabled_hint = (
            f"skill '{name}' is disabled; enable it to view its contents"
            if self._store.is_disabled(name)
            else ""
        )

        if file_path:
            content = self._store.read_file(name, file_path)
            if content is None:
                return ToolResult(
                    success=False,
                    error=disabled_hint or f"File '{file_path}' not found in skill '{name}'",
                )
            return ToolResult(success=True, output=content)

        content = self._store.read_skill(name)
        if content is None:
            return ToolResult(success=False, error=disabled_hint or f"Skill '{name}' not found")

        files = self._store.list_files(name)
        output = content
        if files:
            output += "\n\n---\nLinked files:\n" + "\n".join(f"  - {f}" for f in files)

        try:
            dep_notice = await self._precheck_deps(name, content, ctx)
        except Exception as e:  # never let dep precheck break a plain view
            logger.warning("skill_view dependency precheck failed for {}: {}", name, e)
            dep_notice = ""
        if dep_notice:
            output += dep_notice

        return ToolResult(success=True, output=output)


class SkillManageTool(Tool):
    name = "skill_manage"
    risk_level = "dangerous"
    description = (
        "Create, edit, patch, or delete skills. Use this to capture reusable knowledge "
        "from completed tasks. Actions: create, edit, patch, delete, write_file, remove_file."
    )
    parameters = {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["create", "edit", "patch", "delete", "write_file", "remove_file"],
                "description": "The operation to perform",
            },
            "name": {"type": "string", "description": "Skill name (lowercase, alphanumeric, hyphens)"},
            "category": {"type": "string", "description": "Optional category directory for create"},
            "content": {
                "type": "string",
                "description": "SKILL.md content with YAML frontmatter (for create/edit), or file content (for write_file)",
            },
            "file_path": {
                "type": "string",
                "description": "Supporting file path for patch/write_file/remove_file (e.g. 'references/notes.md')",
            },
            "old_text": {"type": "string", "description": "Text to find (for patch action)"},
            "new_text": {"type": "string", "description": "Replacement text (for patch action)"},
        },
        "required": ["action", "name"],
    }

    def __init__(self, store: SkillStore):
        self._store = store

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params["action"]
        name = params["name"]

        if action == "create":
            content = params.get("content", "")
            if not content:
                return ToolResult(success=False, error="content is required for create")
            err = self._store.create_skill(name, content, category=params.get("category", ""))
            if err:
                return ToolResult(success=False, error=err)
            return ToolResult(success=True, output=f"Skill '{name}' created.")

        elif action == "edit":
            content = params.get("content", "")
            if not content:
                return ToolResult(success=False, error="content is required for edit")
            err = self._store.update_skill(name, content)
            if err:
                return ToolResult(success=False, error=err)
            return ToolResult(success=True, output=f"Skill '{name}' updated.")

        elif action == "patch":
            old_text = params.get("old_text", "")
            new_text = params.get("new_text", "")
            if not old_text:
                return ToolResult(success=False, error="old_text is required for patch")
            err = self._store.patch_skill(name, old_text, new_text, file_path=params.get("file_path", ""))
            if err:
                return ToolResult(success=False, error=err)
            return ToolResult(success=True, output=f"Skill '{name}' patched.")

        elif action == "delete":
            err = self._store.delete_skill(name)
            if err:
                return ToolResult(success=False, error=err)
            return ToolResult(success=True, output=f"Skill '{name}' deleted.")

        elif action == "write_file":
            file_path = params.get("file_path", "")
            content = params.get("content", "")
            if not file_path or not content:
                return ToolResult(success=False, error="file_path and content are required for write_file")
            err = self._store.write_file(name, file_path, content)
            if err:
                return ToolResult(success=False, error=err)
            return ToolResult(success=True, output=f"File '{file_path}' written to skill '{name}'.")

        elif action == "remove_file":
            file_path = params.get("file_path", "")
            if not file_path:
                return ToolResult(success=False, error="file_path is required for remove_file")
            err = self._store.remove_file(name, file_path)
            if err:
                return ToolResult(success=False, error=err)
            return ToolResult(success=True, output=f"File '{file_path}' removed from skill '{name}'.")

        return ToolResult(success=False, error=f"Unknown action '{action}'")
