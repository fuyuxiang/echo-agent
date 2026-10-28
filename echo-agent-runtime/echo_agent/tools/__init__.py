"""Stable public API for framework-level tool contracts.

``Tool``, ``ToolResult`` and ``ToolExecutionContext`` are the contracts every
tool-providing subsystem (mcp, plugins, evolution, security) builds against.
They live here — below the agent core — so that providing a tool never
requires importing the orchestrator. Concrete built-in tools remain in
``echo_agent.agent.tools``.

Extensions should import these contracts from ``echo_agent.tools``. The
``echo_agent.tools.base`` module contains their implementation, while
``echo_agent.agent.tools.base`` is retained as a backward-compatibility shim.
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
