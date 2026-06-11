"""Backward-compat shim — the tool contracts moved to ``echo_agent.tools.base``.

Kept so existing imports (``echo_agent.agent.tools.base``) keep working;
new code should import from ``echo_agent.tools``.
"""

from echo_agent.tools.base import (  # noqa: F401
    Tool,
    ToolExecutionContext,
    ToolResult,
    _validate_json_schema,
    build_idempotency_key,
)

__all__ = [
    "Tool",
    "ToolExecutionContext",
    "ToolResult",
    "build_idempotency_key",
]
