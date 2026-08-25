"""MCP manager — orchestrates multiple MCP server connections and tool registration.

Owns the whole lifecycle of every configured MCP server: connect (with retry),
discover and register its tools, watch the connection, and on loss unregister
those tools and rebuild from scratch. The invariant it exists to hold is that
**the registry advertises a tool only while that tool is actually callable** —
the previous version registered tools once and never revisited the decision, so
a server that died left its tools listed and every call failed.
"""

from __future__ import annotations

import asyncio
import os
import re
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.config.schema import MCPServerConfig
from echo_agent.mcp.client import MCPClient
from echo_agent.mcp.security import validate_mcp_tools
from echo_agent.mcp.tool_adapter import MCPToolAdapter
from echo_agent.mcp.transport import StdioTransport, StreamableHttpTransport

_RECONNECT_DELAYS = (1, 2, 4, 8, 16, 30, 60)
_MAX_RECONNECT_ATTEMPTS = 5

#: Matches ``${VAR}`` and bare ``$VAR`` references in config strings.
_ENV_VAR_RE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)")


class MCPManager:

    def __init__(self, workspace: Path, security_policy: str = "block"):
        self._workspace = workspace
        self._security_policy = security_policy
        self._clients: dict[str, MCPClient] = {}
        self._configs: dict[str, MCPServerConfig] = {}
        self._registered_tools: dict[str, list[str]] = {}
        # Not keyed by server: the resource/prompt tools span every connection and
        # take a `server` parameter, so they are withdrawn only when MCP as a
        # whole shuts down.
        self._agent_tool_names: list[str] = []
        self._registry: ToolRegistry | None = None
        # One supervisor task per server, watching for connection loss.
        self._supervisors: dict[str, asyncio.Task] = {}
        # Serialises registry mutations so a supervisor re-registering cannot
        # interleave with discovery or shutdown.
        self._registry_lock = asyncio.Lock()
        self._stopping = False

    async def start_all(self, servers: dict[str, MCPServerConfig]) -> None:
        self._stopping = False
        enabled = [(name, cfg) for name, cfg in servers.items() if cfg.enabled]
        for name, cfg in servers.items():
            if not cfg.enabled:
                logger.debug("MCP server '{}' disabled, skipping", name)

        for name, cfg in enabled:
            self._configs[name] = cfg

        if not enabled:
            return

        results = await asyncio.gather(
            *(self._connect_server(name, cfg) for name, cfg in enabled),
            return_exceptions=True,
        )
        for (name, _cfg), result in zip(enabled, results):
            if isinstance(result, BaseException):
                logger.error("Failed to connect MCP server '{}': {}", name, result)

    async def stop_all(self) -> None:
        self._stopping = True

        for task in self._supervisors.values():
            task.cancel()
        if self._supervisors:
            await asyncio.gather(*self._supervisors.values(), return_exceptions=True)
        self._supervisors.clear()

        clients = list(self._clients.values())
        if clients:
            await asyncio.gather(*(c.disconnect() for c in clients), return_exceptions=True)
        self._clients.clear()

        # Withdraw the tools. stop_all used to clear its own bookkeeping without
        # touching the registry, so after shutdown the model was still offered
        # every MCP tool and every call was guaranteed to fail. _registered_tools
        # was written and never read — this is the read.
        async with self._registry_lock:
            for name in list(self._registered_tools):
                self._unregister_server_tools(name)
            self._registered_tools.clear()
            # The resource/prompt tools go too: with no connection left they can
            # only fail, and leaving them registered is the same defect as
            # leaving the per-server tools behind.
            registry = self._registry
            if registry is not None:
                for tool_name in self._agent_tool_names:
                    registry.unregister(tool_name)
            self._agent_tool_names.clear()

        logger.info("All MCP servers disconnected")

    async def discover_tools(self, registry: ToolRegistry) -> int:
        self._registry = registry
        total = 0

        async with self._registry_lock:
            for name, client in list(self._clients.items()):
                if not client.is_connected:
                    continue
                try:
                    total += await self._register_server_tools(name, client, registry)
                except Exception as e:
                    logger.error("Tool discovery failed for '{}': {}", name, e)

        self._register_agent_tools(registry)
        logger.info("Discovered {} MCP tools from {} servers", total, len(self._clients))
        return total

    def _register_agent_tools(self, registry: ToolRegistry) -> None:
        """Expose resources/* and prompts/* to the agent.

        These are per-manager rather than per-server (they take a ``server``
        parameter) so connecting five MCP servers adds two tool definitions, not
        ten. Registered only when at least one server is connected: a tool the
        model can see but never use costs context on every turn for nothing.
        """
        from echo_agent.mcp.agent_tools import MCPPromptsTool, MCPResourcesTool

        if not self.connected_servers:
            return
        for tool_cls in (MCPResourcesTool, MCPPromptsTool):
            if registry.has(tool_cls.name):
                continue
            registry.register(tool_cls(self))
            self._agent_tool_names.append(tool_cls.name)

    @property
    def connected_servers(self) -> list[str]:
        return [name for name, client in self._clients.items() if client.is_connected]

    def health_report(self) -> dict[str, dict[str, Any]]:
        """Per-server connection state and tool count.

        Reported from live client state rather than from the config, so "MCP is
        healthy" cannot mean merely "MCP is configured".
        """
        report: dict[str, dict[str, Any]] = {}
        for name, cfg in self._configs.items():
            client = self._clients.get(name)
            connected = bool(client and client.is_connected)
            report[name] = {
                "connected": connected,
                "transport": "http" if cfg.url else "stdio",
                "trust_level": cfg.trust_level,
                "tools": len(self._registered_tools.get(name, [])),
                "protocol_version": client.protocol_version if client else None,
            }
        return report

    # ── connection lifecycle ────────────────────────────────────────────────

    async def _connect_server(self, name: str, cfg: MCPServerConfig) -> None:
        """Connect to *name*, retrying with a fresh transport each attempt.

        Two bugs shaped this. The old loop reused one Client and one Transport
        across all five attempts without closing the failed one first, so a
        stdio server that spawned but failed the handshake left an orphan child
        process behind on every retry. And on final failure it raised without
        closing anything at all, with the client never entering ``_clients``, so
        ``stop_all`` could never reach it — a leak with no owner.
        """
        last_error: Exception | None = None

        for attempt in range(_MAX_RECONNECT_ATTEMPTS):
            if self._stopping:
                return

            transport: Any = None
            client: MCPClient | None = None
            try:
                transport = await self._create_transport(name, cfg)
                client = MCPClient(name, transport)
                await client.connect(timeout=cfg.connect_timeout)

                self._clients[name] = client
                if isinstance(transport, StreamableHttpTransport):
                    # Server→client traffic (notably tools/list_changed) can only
                    # arrive over this stream.
                    await transport.open_notification_stream()
                client.set_notification_handler(
                    lambda method, params, _n=name: self._on_notification(_n, method, params)
                )
                self._start_supervisor(name)
                logger.info("Connected to MCP server '{}'", name)
                return

            except Exception as e:
                last_error = e
                # Always tear down this attempt's resources before trying again.
                await self._safe_teardown(name, client, transport)

                if attempt < _MAX_RECONNECT_ATTEMPTS - 1:
                    delay = _RECONNECT_DELAYS[min(attempt, len(_RECONNECT_DELAYS) - 1)]
                    logger.warning(
                        "MCP '{}' connect attempt {} failed: {}. Retrying in {}s",
                        name, attempt + 1, e, delay,
                    )
                    await asyncio.sleep(delay)

        raise ConnectionError(
            f"Failed to connect to MCP server '{name}' after "
            f"{_MAX_RECONNECT_ATTEMPTS} attempts: {last_error}"
        )

    async def _safe_teardown(
        self, name: str, client: MCPClient | None, transport: Any,
    ) -> None:
        """Release a half-built connection without masking the original error."""
        if client is not None:
            try:
                await client.disconnect()
                return  # disconnect() closes the transport
            except Exception as e:
                logger.debug("MCP '{}' client teardown failed: {}", name, e)
        if transport is not None:
            try:
                await transport.close()
            except Exception as e:
                logger.debug("MCP '{}' transport teardown failed: {}", name, e)

    def _start_supervisor(self, name: str) -> None:
        existing = self._supervisors.get(name)
        if existing and not existing.done():
            return
        self._supervisors[name] = asyncio.create_task(self._supervise(name))

    async def _try_reauthorize(self, name: str) -> bool:
        """Refresh this server's OAuth token and apply it to the live transport.

        Called when the server answered 401/403. A token can be revoked or
        invalidated long before its nominal expiry, and nothing reacted to that:
        the 401 surfaced as a generic connection error, the stored token stayed
        on disk, and every reconnect replayed the same dead credential until
        someone deleted the file by hand.

        Returns True when a new token was obtained and installed. Only the
        non-interactive refresh path is attempted — starting a browser
        authorization from a background supervisor would pop a window at an
        arbitrary moment, so a server needing full re-consent is left down with
        an explicit log line instead.
        """
        cfg = self._configs.get(name)
        if cfg is None or cfg.auth != "oauth" or not cfg.url:
            return False

        try:
            from echo_agent.mcp.oauth import MCPOAuthClient

            oauth = MCPOAuthClient(name, cfg.url, self._workspace / "data" / "mcp_tokens")
            token = await oauth.refresh_after_401()
        except Exception as e:
            logger.warning("MCP '{}' token refresh failed: {}", name, e)
            return False

        if not token:
            logger.error(
                "MCP server '{}' rejected our credentials and the token could not be "
                "refreshed. Re-authorize it interactively (restart with the server "
                "enabled) to restore access.", name,
            )
            return False

        # The refreshed token is persisted by the OAuth client, and the reconnect
        # that follows builds a new transport via ensure_token(), which reads it
        # back. Nothing needs to be pushed into the outgoing transport — it is
        # about to be discarded.
        #
        # Forcing the refresh here rather than relying on ensure_token()'s own
        # expiry check is the point: a *revoked* token is not expired, so
        # ensure_token would happily reload the same rejected credential.
        logger.info("MCP '{}' access token refreshed after an authorization failure", name)
        return True

    async def _supervise(self, name: str) -> None:
        """Watch one server and rebuild the connection when it drops.

        This is the piece the documentation already promised and the code never
        had: reconnection existed only inside the *initial* connect loop, so once
        a running server died its tools stayed registered and permanently broken
        until the process restarted.
        """
        try:
            while not self._stopping:
                await asyncio.sleep(1)
                client = self._clients.get(name)
                if client is None:
                    return
                if client.is_connected:
                    continue

                logger.warning("MCP server '{}' lost its connection — rebuilding", name)
                # A rebuild caused by a rejected credential needs a fresh token,
                # not just a fresh socket. Read off the transport, which is where
                # the 401 was actually observed.
                transport = getattr(client, "_transport", None)
                if getattr(transport, "auth_failed", False):
                    await self._try_reauthorize(name)

                await self._teardown_server(name)
                if self._stopping:
                    return

                cfg = self._configs.get(name)
                if cfg is None or not cfg.enabled:
                    return
                try:
                    await self._connect_server(name, cfg)
                except Exception as e:
                    logger.error("MCP server '{}' could not be reconnected: {}", name, e)
                    return

                registry = self._registry
                client = self._clients.get(name)
                if registry is not None and client is not None:
                    async with self._registry_lock:
                        try:
                            count = await self._register_server_tools(name, client, registry)
                            logger.info(
                                "Re-registered {} tools for MCP server '{}'", count, name,
                            )
                        except Exception as e:
                            logger.error("Re-discovery failed for '{}': {}", name, e)
                return  # the new connection has its own supervisor
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.error("MCP supervisor for '{}' failed: {}", name, e)

    async def _teardown_server(self, name: str) -> None:
        """Disconnect one server and withdraw its tools."""
        client = self._clients.pop(name, None)
        if client is not None:
            try:
                await client.disconnect()
            except Exception as e:
                logger.debug("Error disconnecting MCP '{}': {}", name, e)
        async with self._registry_lock:
            self._unregister_server_tools(name)

    def _unregister_server_tools(self, name: str) -> None:
        registry = self._registry
        tool_names = self._registered_tools.pop(name, [])
        if registry is None or not tool_names:
            return
        for tool_name in tool_names:
            registry.unregister(tool_name)
        logger.info("Withdrew {} tools from MCP server '{}'", len(tool_names), name)

    async def _on_notification(self, name: str, method: str, params: dict[str, Any]) -> None:
        """React to a server notification.

        ``tools/list_changed`` was queued and never consumed, which made the
        notification pointless: a server adding or removing a tool had no way to
        inform us. Now it triggers re-discovery, which is the whole reason the
        notification exists.
        """
        if method != "notifications/tools/list_changed":
            return
        registry = self._registry
        client = self._clients.get(name)
        if registry is None or client is None or not client.is_connected:
            return

        logger.info("MCP server '{}' reported a tool list change — re-discovering", name)
        async with self._registry_lock:
            self._unregister_server_tools(name)
            try:
                await self._register_server_tools(name, client, registry)
            except Exception as e:
                logger.error("Re-discovery after list_changed failed for '{}': {}", name, e)

    # ── transports ──────────────────────────────────────────────────────────

    async def _create_transport(self, name: str, cfg: MCPServerConfig) -> Any:
        if cfg.url and cfg.command:
            # Both configured used to mean "URL silently wins", so a typo in one
            # field produced a connection to the other with no indication why.
            raise ValueError(
                f"MCP server '{name}' sets both 'url' and 'command' — configure exactly one"
            )

        if cfg.url:
            await self._check_url_policy(name, cfg.url)
            headers = self._resolve_env_vars(cfg.headers, name)
            if cfg.auth == "oauth":
                headers["Authorization"] = f"Bearer {await self._acquire_oauth_token(name, cfg)}"
            transport = StreamableHttpTransport(url=cfg.url, headers=headers)
            # Bounds a single POST. Distinct from connect_timeout, which bounds
            # only connection establishment.
            transport.set_call_timeout(float(cfg.timeout))
            return transport

        if cfg.command:
            env = self._resolve_env_vars(cfg.env, name)
            return StdioTransport(command=cfg.command, args=cfg.args, env=env)

        raise ValueError(f"MCP server '{name}' has neither 'url' nor 'command' configured")

    async def _check_url_policy(self, name: str, url: str) -> None:
        """Run an MCP server URL through the shared outbound-network guard.

        The URL comes from the config file, so this is a weaker threat model than
        ``web_fetch`` — but the project has one gate for outbound HTTP and this
        path bypassed it entirely, accepting any scheme including
        ``file:///etc/passwd``. Under OAuth it stops being purely administrator
        input anyway, because the flow follows endpoints the server publishes.

        A private-address rejection is downgraded to a warning: pointing MCP at a
        server on the LAN or a dev box is a normal deployment, unlike a
        model-supplied fetch target.
        """
        from echo_agent.security.net_guard import ALLOWED_SCHEMES, check_url_ssrf
        from urllib.parse import urlparse

        scheme = urlparse(url).scheme
        if scheme not in ALLOWED_SCHEMES:
            raise ValueError(
                f"MCP server '{name}' has URL scheme '{scheme}', which is not permitted "
                f"(allowed: {', '.join(ALLOWED_SCHEMES)})"
            )
        if error := await check_url_ssrf(url):
            logger.warning("MCP server '{}' URL is not publicly routable: {}", name, error)

    async def _acquire_oauth_token(self, name: str, cfg: MCPServerConfig) -> str:
        from echo_agent.mcp.oauth import MCPOAuthClient

        oauth = MCPOAuthClient(name, cfg.url, self._workspace / "data" / "mcp_tokens")
        return await oauth.ensure_token()

    # ── tool registration ───────────────────────────────────────────────────

    async def _register_server_tools(
        self, name: str, client: MCPClient, registry: ToolRegistry,
    ) -> int:
        cfg = self._configs.get(name)
        raw_tools = await client.list_tools(
            timeout=float(cfg.connect_timeout) if cfg else 30.0
        )

        # Read the registry's current names on every pass rather than caching a
        # set from start-up: a re-registration after reconnect has to see the
        # world as it is now, including tools other servers registered since.
        # This server's own previous names are already withdrawn by the caller.
        existing_names = set(registry.tool_names)

        accepted = validate_mcp_tools(
            server_name=name,
            tools=raw_tools,
            builtin_names=existing_names,
            include_filter=(cfg.tools_include or None) if cfg else None,
            exclude_filter=(cfg.tools_exclude or None) if cfg else None,
            policy=self._security_policy,
        )

        registered_names: list[str] = []
        for entry in accepted:
            adapter = MCPToolAdapter(
                server_name=name,
                mcp_tool=entry.declaration,
                client=client,
                trust_level=cfg.trust_level if cfg else "untrusted",
                registered_name=entry.registered_name,
            )
            if cfg:
                adapter.timeout_seconds = cfg.timeout
            registry.register(adapter)
            registered_names.append(adapter.name)

        self._registered_tools[name] = registered_names
        logger.info(
            "Registered {} tools from MCP server '{}' (trust_level={})",
            len(registered_names), name, cfg.trust_level if cfg else "untrusted",
        )
        return len(registered_names)

    def _resolve_env_vars(self, mapping: dict[str, str], server_name: str = "") -> dict[str, str]:
        """Expand ``${VAR}`` and ``$VAR`` references against the environment.

        An unset variable raises instead of passing the literal text through. The
        old behaviour sent ``Authorization: Bearer ${MCP_TOKEN}`` verbatim when
        the variable was missing, so the failure surfaced as an opaque 401 from
        the server rather than as the configuration error it was.
        """
        resolved: dict[str, str] = {}
        for key, value in mapping.items():
            missing: list[str] = []

            def _substitute(match: re.Match[str]) -> str:
                var = match.group(1) or match.group(2)
                if var in os.environ:
                    return os.environ[var]
                missing.append(var)
                return ""

            expanded = _ENV_VAR_RE.sub(_substitute, value)
            if missing:
                where = f" for MCP server '{server_name}'" if server_name else ""
                raise ValueError(
                    f"Environment variable(s) {', '.join(sorted(set(missing)))} referenced by "
                    f"'{key}'{where} are not set"
                )
            resolved[key] = expanded
        return resolved
