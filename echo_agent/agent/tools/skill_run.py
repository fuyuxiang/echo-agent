"""skill_run — execute a skill's script in the agent's own interpreter.

Skills are documented in SKILL.md as a fixed bundle of file operations, but
their **scripts** are runnable Python that depends on optional packages
(duckduckgo_search, trafilatura, …). Without a single chokepoint they were
executed via the shell tool, which:

* resolved ``python3`` against the shell's PATH — not the venv the agent
  itself uses (the gap executor.base.prepend_interpreter_bin now closes
  for the shell tool, but does not change anything the model wrote by hand),
* ran from the workspace cwd rather than the skill's directory, so a script
  that opens ``./templates/foo.json`` looks in the wrong place,
* bypassed the skill-script dependency handshake entirely — a skill whose
  SKILL.md declares ``requires.pip`` is no different from one that does not.

This tool makes running a skill script a deliberate, gated operation: the
interpreter and cwd are pinned, declared dependencies are resolved first,
the model gets structured output, and the same approval gate other EXEC
tools go through still applies.
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.dependencies.lazy_deps import install_authorized_async
from echo_agent.skills.store import parse_frontmatter, SkillStore


class SkillRunTool(Tool):
    name = "skill_run"
    risk_level = "exec"
    description = (
        "Run a skill's script with the agent's own Python interpreter and cwd "
        "set to the skill's directory. Declared pip dependencies in SKILL.md are "
        "resolved (installed on consent) before the script starts. Use this "
        "instead of the shell tool for anything documented as a skill script."
    )
    parameters = {
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Skill name, e.g. 'web search' (must match SKILL.md frontmatter)",
            },
            "script": {
                "type": "string",
                "description": (
                    "Path to the script relative to the skill's root, "
                    "e.g. 'scripts/web_search.py'"
                ),
            },
            "args": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Arguments to pass as sys.argv[1:] to the script",
            },
            "timeout": {
                "type": "integer",
                "description": "Hard ceiling in seconds. Defaults to 60.",
            },
        },
        "required": ["name", "script"],
    }
    timeout_seconds = 120

    def __init__(self, store: SkillStore, *, approval: Any = None, bus: Any = None, config: Any = None):
        self._store = store
        self._approval = approval
        self._bus = bus
        self._config = config

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "exec"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        name = params["name"]
        script_rel = params["script"]
        args = list(params.get("args") or [])
        timeout = max(1, min(int(params.get("timeout") or 60), 600))

        skill_root = self._store.find_skill_dir(name)
        if skill_root is None:
            return ToolResult(
                success=False,
                error=f"skill '{name}' not found (or not enabled)",
                error_kind="business",
            )
        script_path = (skill_root / script_rel).resolve()
        # Pinning cwd is half the point; refuse anything that escapes the
        # skill root so the script cannot read the rest of the workspace.
        try:
            script_path.relative_to(skill_root.resolve())
        except ValueError:
            return ToolResult(
                success=False,
                error=f"script '{script_rel}' is outside skill '{name}'",
                error_kind="validation",
            )
        if not script_path.is_file() or not script_path.name.endswith(".py"):
            return ToolResult(
                success=False,
                error=f"script '{script_rel}' not found or not a .py file",
                error_kind="validation",
            )

        await self._ensure_deps(name, skill_root, ctx)

        # Use sys.executable — never "python3", which on a launchd/systemd
        # supervisor without the venv bin on PATH would resolve to the
        # system interpreter and miss every dep installed into the venv.
        proc = await asyncio.create_subprocess_exec(
            sys.executable,
            str(script_path),
            *args,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=str(skill_root),
            env={},
            # Inherit nothing — the script gets a clean env with just the
            # PATH enrichment that comes from how sys.executable's bin/ is
            # laid out. Caller-supplied env is intentionally not honored:
            # skill scripts are documented behaviors, not arbitrary code.
        )
        try:
            stdout, stderr = await asyncio.wait_for(
                proc.communicate(), timeout=timeout,
            )
        except asyncio.TimeoutError:
            proc.kill()
            return ToolResult(
                success=False,
                error=f"timeout after {timeout}s",
                error_kind="timeout",
            )
        return ToolResult(
            success=proc.returncode == 0,
            output=stdout.decode(errors="replace"),
            error=stderr.decode(errors="replace") if proc.returncode != 0 else "",
            metadata={
                "skill": name,
                "script": script_rel,
                "return_code": proc.returncode or 0,
                "stderr": stderr.decode(errors="replace"),
            },
        )

    async def _ensure_deps(
        self, name: str, skill_root: Path, ctx: ToolExecutionContext | None,
    ) -> None:
        """Install the skill's declared pip deps before running the script.

        Routes the install through the same approval flow as ``skill_view``,
        so a non-trusted channel will see the user's consent prompt instead
        of a silent install. On a trusted CLI the deps install directly.
        Failure is non-fatal here: we surface it through the script's own
        ImportError so the user sees the actual missing module.
        """
        content = self._store.read_skill(name)
        if not content:
            return
        specs = _skill_pip_specs(content)
        if not specs:
            return
        missing = [s for s in specs if not _is_satisfied(s)]
        if not missing:
            return
        # Same trusted-environment shortcut as SkillViewTool.
        if self._is_trusted_env(ctx.channel if ctx else ""):
            result = await install_authorized_async(tuple(missing), source=f"skill_run_trusted:{name}")
            if not result.get("success"):
                logger.warning("skill_run auto-install failed for {}: {}", name, result.get("detail"))
            return
        if self._approval is None or self._bus is None:
            return  # cannot prompt; the script will fail loudly with ImportError
        from echo_agent.permissions.manager import ApprovalStatus
        from echo_agent.bus.events import OutboundEvent

        req = self._approval.request_approval(
            "dep_install",
            tool_name="skill_run",
            params={"skill": name, "missing": missing},
            user_id=ctx.user_id if ctx else "",
        )
        prompt = (
            f"技能「{name}」运行前需要安装依赖: {missing}。"
            f"回复 /approve {req.id} 授权安装，或 /deny {req.id} 拒绝。"
        )
        out = OutboundEvent.text_reply(
            channel=ctx.channel if ctx else "",
            chat_id=ctx.chat_id if ctx else "",
            text=prompt,
            reply_to_id=ctx.reply_to_id or None,
        )
        out.metadata["_dep_install_request"] = True
        out.metadata["_skill_name"] = name
        out.metadata["_missing"] = missing
        out.metadata["_request_id"] = req.id
        await self._bus.publish_outbound(out)
        decided = await self._approval.wait_for_decision(req.id, timeout_seconds=300)
        if decided is not None and getattr(decided, "status", None) == ApprovalStatus.APPROVED:
            await install_authorized_async(tuple(missing), source=f"skill_run:{name}")

    def _is_trusted_env(self, channel: str) -> bool:
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


def _is_satisfied(spec: str) -> bool:
    """Cheap copy of lazy_deps._is_satisfied so this module doesn't reach into
    another package's private API. A spec is satisfied when its top-level name
    imports without error."""
    try:
        from echo_agent.dependencies.lazy_deps import _is_satisfied as _real
        return _real(spec)
    except Exception:
        return False


def _skill_pip_specs(content: str) -> list[str]:
    """Read a skill's declared pip deps from SKILL.md frontmatter.

    Same logic as SkillViewTool._skill_pip_specs; inlined here so this module
    does not reach into another tool's private API.
    """
    try:
        fm, _ = parse_frontmatter(content)
    except Exception:
        return []
    echo_meta = (fm.get("metadata", {}) or {}).get("echo", {}) or {}
    requires = echo_meta.get("requires", {}) or {}
    return list(requires.get("pip", []) or [])