"""Plugin context — the API surface available to plugins during activation."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable, Coroutine, TYPE_CHECKING

from loguru import logger

from echo_agent.plugins.errors import PluginPermissionError
from echo_agent.plugins.hooks import HookCallback, HookRegistry

if TYPE_CHECKING:
    from echo_agent.tools import Tool
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
        tool_registration_allowed: Callable[[], bool] | None = None,
        hook_registration_allowed: Callable[[], bool] | None = None,
        registration_mode: str = "compat",
    ) -> None:
        self._plugin_name = plugin_name
        self._config = config
        self._workspace = workspace
        self._bus = bus
        self._tool_registry = tool_registry
        self._hook_registry = hook_registry
        self._provider = provider
        self._plugin_config = plugin_config or {}
        self._tool_registration_allowed = tool_registration_allowed
        self._hook_registration_allowed = hook_registration_allowed
        self._registration_mode = registration_mode
        self._logger = logger.bind(plugin=plugin_name)
        self._registered_tools: list[str] = []
        # Keep the exact instances as well as their public names.  Lifecycle
        # ownership cannot safely be recovered from ToolRegistry by name at
        # shutdown: another owner may have explicitly replaced/unregistered a
        # tool in the meantime, in which case a lookup would close the wrong
        # object or leak the original one.
        self._registered_tool_instances: list["Tool"] = []
        self._denied_tool_instances: list["Tool"] = []
        # Tool ownership transfers to the host before registry admission.  A
        # collision or invalid name raises from ToolRegistry.register(); keeping
        # the exact object here lets activation rollback close resources that
        # were constructed by the plugin but never became registry-visible.
        self._pending_tool_instances: list["Tool"] = []
        self._registered_hooks: list[str] = []
        self._registered_inbound_handlers: list[InboundHandler] = []
        self._denied_registrations: set[str] = set()

    # ── Tool registration ──────────────────────────────────────────────────

    def register_tool(self, tool: "Tool") -> None:
        """Register a Tool instance into the agent's tool registry.

        The tool goes through the normal ApprovalGate and security flow.
        """
        from echo_agent.tools import Tool as ToolBase

        if not isinstance(tool, ToolBase):
            raise TypeError(
                f"Expected a Tool instance, got {type(tool).__name__}. "
                "Plugin tools must inherit from echo_agent.tools.Tool."
            )
        if not self._registration_allowed(
            "tool.register", self._tool_registration_allowed,
        ):
            if all(owned is not tool for owned in self._denied_tool_instances):
                self._denied_tool_instances.append(tool)
            self._raise_if_strict("tool.register")
            return
        if all(owned is not tool for owned in self._pending_tool_instances):
            self._pending_tool_instances.append(tool)
        self._tool_registry.register(tool)
        self._pending_tool_instances = [
            owned for owned in self._pending_tool_instances if owned is not tool
        ]
        if tool.name not in self._registered_tools:
            self._registered_tools.append(tool.name)
            self._registered_tool_instances.append(tool)
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
        if not self._registration_allowed(
            "hook.register", self._hook_registration_allowed,
        ):
            self._raise_if_strict("hook.register")
            return
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
        self._registered_inbound_handlers.append(handler)

    def _registration_allowed(
        self,
        permission: str,
        gate: Callable[[], bool] | None,
    ) -> bool:
        """Enforce registry admission before a resource becomes host-visible."""
        if gate is None or gate():
            return True
        self._denied_registrations.add(permission)
        self._logger.warning("Blocked undeclared registration: {}", permission)
        return False

    def _raise_if_strict(self, permission: str) -> None:
        if self._registration_mode == "strict":
            raise PluginPermissionError(
                self._plugin_name, f"permission denied for {permission}",
            )

    def unsubscribe_inbound(self, handler: InboundHandler) -> None:
        """Release one inbound subscription previously owned by this plugin."""
        if handler not in self._registered_inbound_handlers:
            return
        self._bus.unsubscribe_inbound(handler)
        self._registered_inbound_handlers.remove(handler)

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
    def registered_tool_instances(self) -> list["Tool"]:
        """Exact tool objects owned by this plugin, in registration order."""
        return list(self._registered_tool_instances)

    @property
    def denied_tool_instances(self) -> list["Tool"]:
        """Constructed tools refused admission and still needing cleanup."""
        return list(self._denied_tool_instances)

    @property
    def owned_tool_instances(self) -> list["Tool"]:
        """All exact tool objects whose lifecycle was handed to this context."""
        seen: set[int] = set()
        tools: list["Tool"] = []
        for tool in (
            *self._registered_tool_instances,
            *self._denied_tool_instances,
            *self._pending_tool_instances,
        ):
            if id(tool) not in seen:
                seen.add(id(tool))
                tools.append(tool)
        return tools

    @property
    def registered_hooks(self) -> list[str]:
        return list(self._registered_hooks)

    @property
    def registered_inbound_handlers(self) -> list[InboundHandler]:
        """Inbound observers owned by this plugin, in registration order."""
        return list(self._registered_inbound_handlers)

    @property
    def denied_registrations(self) -> set[str]:
        """Actual permission violations observed at registration boundaries."""
        return set(self._denied_registrations)
