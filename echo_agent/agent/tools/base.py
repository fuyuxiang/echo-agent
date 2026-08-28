"""Backward-compatibility shim for the former tool-contract import path.

Kept so existing imports (``echo_agent.agent.tools.base``) keep working;
extensions and new code should import the public API from ``echo_agent.tools``.
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
