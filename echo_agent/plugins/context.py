"""Plugin context — the API surface available to plugins during activation."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable, Coroutine, TYPE_CHECKING

from loguru import logger

from echo_agent.plugins.hooks import HookCallback, HookRegistry

if TYPE_CHECKING:
    from echo_agent.tools.base import Tool
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.bus.events import InboundEvent, OutboundEvent
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.models.provider import LLMProvider


InboundHandler = Callable[["InboundEvent"], Coroutine[Any, Any, None]]


class PluginContext:
    """The API surface available to plugins during activation.

    Provides controlled access to echo-agent internals without exposing
    raw references that could break internal state.
    """

    def __init__(
        self,
        *,
        plugin_name: str,
        config: "Config",
        workspace: Path,
        bus: "MessageBus",
        tool_registry: "ToolRegistry",
        hook_registry: HookRegistry,
        provider: "LLMProvider | None" = None,
        plugin_config: dict[str, Any] | None = None,
    ) -> None:
        self._plugin_name = plugin_name
        self._config = config
        self._workspace = workspace
        self._bus = bus
        self._tool_registry = tool_registry
        self._hook_registry = hook_registry
        self._provider = provider
        self._plugin_config = plugin_config or {}
        self._logger = logger.bind(plugin=plugin_name)
        self._registered_tools: list[str] = []
        self._registered_hooks: list[str] = []

    # ── Tool registration ──────────────────────────────────────────────────

    def register_tool(self, tool: "Tool") -> None:
        """Register a Tool instance into the agent's tool registry.

        The tool goes through the normal ApprovalGate and security flow.
        """
        from echo_agent.tools.base import Tool as ToolBase

        if not isinstance(tool, ToolBase):
            raise TypeError(
                f"Expected a Tool instance, got {type(tool).__name__}. "
                "Plugin tools must inherit from echo_agent.tools.Tool."
            )
        self._tool_registry.register(tool)
        self._registered_tools.append(tool.name)
        self._logger.debug("Registered tool: {}", tool.name)

    def register_tools(self, tools: list["Tool"]) -> None:
        """Register multiple tools at once."""
        for tool in tools:
            self.register_tool(tool)

    # ── Hook registration ──────────────────────────────────────────────────

    def register_hook(self, hook_name: str, callback: HookCallback) -> None:
        """Register a callback for a lifecycle hook.

        See echo_agent.plugins.hooks.VALID_HOOKS for available hook names.
        """
        self._hook_registry.register(hook_name, callback, plugin=self._plugin_name)
        self._registered_hooks.append(hook_name)
        self._logger.debug("Registered hook: {}", hook_name)

    # ── Event bus access ───────────────────────────────────────────────────

    async def publish_outbound(self, event: "OutboundEvent") -> None:
        """Publish an outbound event (progress, streaming, etc.)."""
        await self._bus.publish_outbound(event)

    def subscribe_inbound(self, handler: InboundHandler) -> None:
        """Subscribe to inbound events (read-only observation).

        The handler is called for every inbound event but cannot modify it.
        """
        self._bus.subscribe_inbound(handler)

    # ── Config access ──────────────────────────────────────────────────────

    @property
    def plugin_config(self) -> dict[str, Any]:
        """Plugin-specific config section from echo-agent.yaml."""
        return self._plugin_config

    @property
    def workspace(self) -> Path:
        """The agent workspace directory."""
        return self._workspace

    @property
    def config(self) -> "Config":
        """Read-only access to the full agent config."""
        return self._config

    # ── LLM access ─────────────────────────────────────────────────────────

    @property
    def llm_provider(self) -> "LLMProvider | None":
        """Access to the LLM provider for plugin-internal inference."""
        return self._provider

    # ── Logging ────────────────────────────────────────────────────────────

    @property
    def log(self) -> Any:
        """A logger scoped to this plugin name."""
        return self._logger

    # ── Introspection ──────────────────────────────────────────────────────

    @property
    def plugin_name(self) -> str:
        return self._plugin_name

    @property
    def registered_tools(self) -> list[str]:
        return list(self._registered_tools)

    @property
    def registered_hooks(self) -> list[str]:
        return list(self._registered_hooks)
