"""Framework-level tool contracts.

``Tool``, ``ToolResult`` and ``ToolExecutionContext`` are the contracts every
tool-providing subsystem (mcp, plugins, evolution, security) builds against.
They live here — below the agent core — so that providing a tool never
requires importing the orchestrator. Concrete built-in tools remain in
``echo_agent.agent.tools``.
"""

from echo_agent.tools.base import (
    Tool,
    ToolExecutionContext,
    ToolResult,
    build_idempotency_key,
)

__all__ = [
    "Tool",
    "ToolExecutionContext",
    "ToolResult",
    "build_idempotency_key",
]
