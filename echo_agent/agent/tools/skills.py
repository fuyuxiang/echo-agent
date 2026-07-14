"""Agent-facing skill tools — skills_list, skill_view, skill_manage.

Provides the LLM with progressive-disclosure access to the skill store
and the ability to create/edit/delete skills for self-learning.
"""

from __future__ import annotations

import json
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.bus.events import OutboundEvent
from echo_agent.dependencies.lazy_deps import (
    INSTALL_TIMEOUT_SECONDS,
    _is_satisfied,
    install_authorized_async,
)
from echo_agent.permissions.manager import ApprovalStatus
from echo_agent.skills.store import SkillStore

# How long to wait for a user's /approve|/deny decision on a dependency
# install before giving up and returning the skill view without the deps.
_DEP_APPROVAL_TIMEOUT_SECONDS = 300


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
    # A view may trigger a dependency install: up to _DEP_APPROVAL_TIMEOUT_SECONDS
    # waiting for consent, then up to INSTALL_TIMEOUT_SECONDS installing. The
    # registry wraps execute() in asyncio.wait_for(timeout_seconds); it must sit
    # above both so a legitimately slow install runs to completion instead of
    # being abandoned mid-write (see lazy_deps.INSTALL_TIMEOUT_SECONDS).
    timeout_seconds = _DEP_APPROVAL_TIMEOUT_SECONDS + INSTALL_TIMEOUT_SECONDS + 30
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

    def __init__(self, store: SkillStore, *, approval: Any = None, bus: Any = None, config: Any = None):
        self._store = store
        self._approval = approval
        self._bus = bus
        self._config = config

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"

    def _is_trusted_env(self, channel: str) -> bool:
        """Mirror ApprovalGate's CLI-auto-approve and trusted-channel exemptions
        without depending on an ApprovalGate instance. Used to skip the approval
        prompt for dependency installs in trusted environments (CLI on personal
        machine, or an explicitly trusted channel) — the same operations exec
        already enjoys. config=None is treated as untrusted (backward compat).
        """
        if self._config is None:
            return False
        try:
            approval_cfg = self._config.permissions.approval
            cli_exempt = (
                self._config.security.profile == "personal_cli"
                and approval_cfg.cli_auto_approve
                and channel in {"cli", "direct", ""}
            )
            return cli_exempt or channel in approval_cfg.trusted_channels
        except Exception:
            return False

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
        """If the skill declares pip deps that are missing, route them through the
        approval closed-loop using the current channel's user. Returns an extra
        text block to append to the skill view (empty when nothing to add).

        Degrades to a no-op (returns "") when approval/bus are not injected or
        there is no originating channel (worker / no-event contexts).
        """
        if self._approval is None or self._bus is None:
            return ""
        if ctx is None or not ctx.channel:
            return ""

        specs = self._skill_pip_specs(name, content)
        if not specs:
            return ""
        missing = [s for s in specs if not _is_satisfied(s)]
        if not missing:
            return ""

        spec_list = " ".join(missing)

        # Trusted-environment exemption: CLI on a personal machine, or an
        # explicitly trusted channel, installs directly without an approval
        # prompt — matching the exemptions exec already gets. The install is
        # still gated by install_authorized (venv + _spec_is_safe), so this
        # is controlled, not a bare install. Untrusted channels fall through
        # to the approval closed-loop below.
        if self._is_trusted_env(ctx.channel):
            result = await install_authorized_async(tuple(missing), source=f"skill_view_trusted:{name}")
            if result.get("success"):
                installed = result.get("installed") or []
                skipped = result.get("skipped") or []
                detail = "、".join(str(x) for x in (list(installed) + list(skipped))) or spec_list
                return f"\n\n---\n依赖已安装:{detail}。现在可以运行该技能脚本。"
            return (
                f"\n\n---\n注意:技能「{name}」依赖 {spec_list} 安装失败:"
                f"{result.get('detail', '未知错误')}。暂时无法运行该技能脚本。"
            )

        req = self._approval.request_approval(
            "dep_install",
            tool_name="skill_view",
            params={"skill": name, "missing": missing},
            user_id=ctx.user_id,
        )

        prompt = (
            f"技能「{name}」需要安装以下依赖才能运行其脚本:\n  {spec_list}\n"
            f"回复 /approve {req.id} 授权安装,或 /deny {req.id} 拒绝。"
        )
        out = OutboundEvent.text_reply(
            channel=ctx.channel,
            chat_id=ctx.chat_id,
            text=prompt,
            reply_to_id=ctx.reply_to_id or None,
        )
        out.metadata["_dep_install_request"] = True
        out.metadata["_skill_name"] = name
        out.metadata["_missing"] = missing
        out.metadata["_request_id"] = req.id
        await self._bus.publish_outbound(out)

        decided = await self._approval.wait_for_decision(
            req.id, timeout_seconds=_DEP_APPROVAL_TIMEOUT_SECONDS
        )
        approved = decided is not None and getattr(decided, "status", None) == ApprovalStatus.APPROVED
        if not approved:
            return (
                f"\n\n---\n注意:技能「{name}」缺少依赖 {spec_list},"
                "用户未授权安装,需授权后才能运行该技能脚本。"
            )

        result = await install_authorized_async(tuple(missing), source=f"skill_view:{name}")
        if result.get("success"):
            installed = result.get("installed") or []
            skipped = result.get("skipped") or []
            detail = "、".join(str(x) for x in (list(installed) + list(skipped))) or spec_list
            return f"\n\n---\n依赖已安装:{detail}。现在可以运行该技能脚本。"
        return (
            f"\n\n---\n注意:技能「{name}」依赖 {spec_list} 安装失败:"
            f"{result.get('detail', '未知错误')}。暂时无法运行该技能脚本。"
        )

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        name = params["name"]
        file_path = params.get("file_path", "")

        if file_path:
            content = self._store.read_file(name, file_path)
            if content is None:
                return ToolResult(success=False, error=f"File '{file_path}' not found in skill '{name}'")
            return ToolResult(success=True, output=content)

        content = self._store.read_skill(name)
        if content is None:
            return ToolResult(success=False, error=f"Skill '{name}' not found")

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
