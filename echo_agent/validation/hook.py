"""post_tool_call hook wiring for post-write validation feedback."""
from __future__ import annotations

from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.plugins.hooks import HookResult
from echo_agent.validation import set_validator
from echo_agent.validation.checkers import default_checkers
from echo_agent.validation.validator import Validator

WRITE_TOOLS: frozenset[str] = frozenset({"write_file", "edit_file", "patch"})


def _extract_path(tool_name: str, arguments: dict) -> str | None:
    if tool_name in ("write_file", "edit_file"):
        return arguments.get("path")
    if tool_name == "patch":
        return arguments.get("file_path")
    return None


def make_post_tool_validation_hook(validator: Validator, workspace: Path):
    async def _hook(result: Any, name: str, arguments: Any, exec_ctx: Any = None) -> HookResult | None:
        try:
            if name not in WRITE_TOOLS:
                return None
            if not getattr(result, "success", False):
                return None
            if not isinstance(arguments, dict):
                return None
            rel = _extract_path(name, arguments)
            if not rel:
                return None
            path = Path(rel)
            if not path.is_absolute():
                path = workspace / path
            diags = await validator.validate(path)
            if not diags:
                return None
            block = validator.format_diagnostics(diags, path.name)
            result.output = f"{result.output}\n\n{block}" if result.output else block
            return HookResult(modified=result)
        except Exception as e:  # fail-open, never block the tool result
            logger.debug("validation post_tool hook failed (fail-open): {}", e)
            return None
    return _hook


def install_validation(config: Any, workspace: Path, hook_registry: Any) -> Validator | None:
    vc = config.validation
    if not vc.enabled:
        return None
    validator = Validator(
        checkers=default_checkers(),
        timeout_sec=vc.timeout_sec,
        max_diagnostics=vc.max_diagnostics,
        max_file_size_kb=vc.max_file_size_kb,
    )
    set_validator(validator)
    hook_registry.register(
        "post_tool_call",
        make_post_tool_validation_hook(validator, Path(workspace)),
        plugin="validation",
    )
    logger.info("Post-write validation enabled (timeout={}s)", vc.timeout_sec)
    return validator
