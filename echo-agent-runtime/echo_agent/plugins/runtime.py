"""Optional, operator-installed conversation extensions; no project protocol lives here."""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

from echo_agent.mcp.tool_adapter import MCPToolAdapter

if TYPE_CHECKING:
    from aiohttp.web import Application
    from echo_agent.bus.delivery import DeliveryResult
    from echo_agent.bus.events import InboundEvent
    from echo_agent.config.schema import Config
    from echo_agent.gateway.auth import GatewayAuth
    from echo_agent.mcp.client import MCPClient
    from echo_agent.mcp.manager import MCPManager


# /***********************************************************
#  *                      本地方法                          *
#  ***********************************************************/

class RuntimeExtensionError(RuntimeError):
    """A critical plugin policy failed; catalog callers must not continue with stale tools."""


class RuntimeExtension:
    """Trusted plugin API. Unlike observational hooks, exceptions propagate.

    One exclusive extension owns conversation policy per runtime. A second
    owner is rejected rather than composing incompatible identity/confirmation
    policies. Ordinary tool/hook plugins remain independently usable.
    """
    system_instructions: str = ""
    session_scope: str | None = None
    memory_namespace: str = ""
    streaming_enabled: bool = True
    allowed_tool_patterns: tuple[str, ...] | None = None

    def validate_config(self, config: Config) -> None:
        """Validate operator settings before accepting extension ownership."""

    async def recover(self) -> None:
        """Recover durable extension state before accepting inbound messages."""

    def bind_mcp_manager(self, manager: MCPManager) -> None:
        """Bind live connections without exposing the full agent loop."""

    def configure_mcp_connection(self, name: str, url: str, headers: dict[str, str]) -> None:
        """Observe resolved connection settings in trusted program code only."""

    def build_mcp_adapter(self, *, server_name: str, mcp_tool: dict[str, Any],
                          client: MCPClient, trust_level: str,
                          registered_name: str) -> MCPToolAdapter | None:
        """Return a model-visible adapter, or None to withhold a private operation."""
        return MCPToolAdapter(server_name=server_name, mcp_tool=mcp_tool, client=client,
                              trust_level=trust_level, registered_name=registered_name)

    async def before_turn(self, manager: MCPManager | None) -> None:
        """Refresh extension resources before inference, with request errors propagated."""

    async def handle_event(self, event: InboundEvent) -> str | None:
        """Optionally handle a native command before invoking the language model."""
        return None

    async def presentation(self, event: InboundEvent) -> str | None:
        """Optionally replace model prose with a program-owned canonical response."""
        return None

    async def record_delivery(self, event: InboundEvent, result: DeliveryResult) -> None:
        """Receive the actual channel result, never an optimistic send invocation."""

    def register_routes(self, app: Application, auth: GatewayAuth) -> None:
        """Mount independently authenticated extension endpoints on the Gateway."""


class RuntimeExtensionSlot:
    """Track a single plugin owner so activation rollback cannot leak policy."""
    def __init__(self) -> None:
        self.owner: str = ""
        self.extension: RuntimeExtension | None = None

    def register(self, owner: str, extension: RuntimeExtension) -> None:
        """Reject conflicting owners, including repeated registration by one plugin."""
        if not isinstance(extension, RuntimeExtension):
            raise TypeError("runtime extension must inherit RuntimeExtension")
        if self.extension is not None:
            raise RuntimeError("runtime extension already registered")
        self.owner, self.extension = owner, extension

    def release(self, owner: str) -> None:
        """Withdraw only the matching owner's extension during rollback/shutdown."""
        if self.owner == owner:
            self.owner, self.extension = "", None
