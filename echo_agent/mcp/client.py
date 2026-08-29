"""MCP client — JSON-RPC 2.0 communication with a single MCP server."""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from typing import Any

from loguru import logger

from echo_agent.mcp.transport import MCPTransport

#: Protocol revision we ask for. 2025-06-18 is the revision whose features this
#: client actually implements end to end (Streamable HTTP with the
#: ``MCP-Protocol-Version`` header, ``structuredContent`` results, cursor
#: pagination). Declaring a version we do not implement — the old code asked for
#: 2024-11-05 while the transport docstring claimed 2025-03-26 — makes the
#: handshake a lie in whichever direction the server trusts it.
PROTOCOL_VERSION = "2025-06-18"

#: Revisions we can still speak if a server negotiates downward. A server is
#: allowed to answer ``initialize`` with an older revision; anything outside this
#: set means we would be guessing at the framing, so we say so rather than
#: proceeding on a version we have never been tested against.
SUPPORTED_PROTOCOL_VERSIONS = frozenset({
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
})

#: Cap on pages walked while draining a cursor-paginated list. A server that
#: returns a fresh cursor forever would otherwise pin this coroutine and grow
#: the accumulator without bound.
_MAX_LIST_PAGES = 100


class MCPClient:

    def __init__(self, name: str, transport: MCPTransport):
        self.name = name
        self._transport = transport
        self._request_id = 0
        self._pending: dict[int, asyncio.Future] = {}
        # Bounded notification queue — silently drop the oldest entry when
        # the producer outpaces the (rare) consumer instead of leaking.
        self._notifications: asyncio.Queue[dict[str, Any]] = asyncio.Queue(maxsize=1024)
        self._reader_task: asyncio.Task | None = None
        self._server_info: dict[str, Any] = {}
        self._server_capabilities: dict[str, Any] = {}
        self._protocol_version = PROTOCOL_VERSION
        self._closed = False
        # Tracks whether the read loop is alive. Without this, `is_connected`
        # reported the transport's opinion only: a reader that had died left the
        # client looking healthy while every request ran to its full timeout.
        self._reader_alive = False
        # Set by the manager so a tools/list_changed notification can trigger
        # re-discovery. Kept as a plain callback rather than having the client
        # import the manager, which would be circular.
        self._notification_handler: Callable[[str, dict[str, Any]], Awaitable[None]] | None = None

    def set_notification_handler(
        self, handler: Callable[[str, dict[str, Any]], Awaitable[None]] | None,
    ) -> None:
        self._notification_handler = handler

    async def connect(self, timeout: float = 60) -> None:
        if hasattr(self._transport, "connect"):
            await self._transport.connect(timeout=timeout)
        self._reader_alive = True
        self._reader_task = asyncio.create_task(self._read_loop())
        # The handshake is the part that actually hangs on a wedged server, so it
        # gets the caller's connect budget. It used to fall back to _request's
        # own 30s default, which meant connect_timeout governed only the
        # milliseconds of process spawn.
        await self.initialize(timeout=timeout)

    async def disconnect(self) -> None:
        self._closed = True
        self._reader_alive = False
        if self._reader_task:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except (asyncio.CancelledError, Exception):
                # disconnect() initiated cancellation; any concurrent reader
                # failure is superseded by failing every pending request below.
                pass
            self._reader_task = None
        try:
            await self._transport.close()
        finally:
            # Fail rather than cancel: a caller awaiting a tool call should get a
            # ConnectionError it can report, not a CancelledError that unwinds
            # its own task as though the caller had asked to stop.
            self._fail_pending(ConnectionError(f"MCP server '{self.name}' disconnected"))

    def _fail_pending(self, error: BaseException) -> None:
        pending, self._pending = self._pending, {}
        for fut in pending.values():
            if not fut.done():
                fut.set_exception(error)

    @property
    def is_connected(self) -> bool:
        return self._transport.is_connected and self._reader_alive and not self._closed

    @property
    def protocol_version(self) -> str:
        return self._protocol_version

    @property
    def server_capabilities(self) -> dict[str, Any]:
        return dict(self._server_capabilities)

    async def initialize(self, timeout: float = 30) -> dict[str, Any]:
        from echo_agent import __version__
        resp = await self._request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                # Deliberately empty. This used to declare {"sampling": {}}, but
                # nothing here ever answered a sampling/createMessage request, so
                # a server taking the declaration at face value would issue one
                # and wait forever. Advertising a capability is a promise; the
                # honest move is to make no promise until there is a handler,
                # an authorization path, and a way to show the user what the
                # server asked our model to generate.
                "capabilities": {},
                "clientInfo": {"name": "echo-agent", "version": __version__},
            },
            timeout=timeout,
        )
        self._server_info = resp.get("serverInfo", {}) or {}
        self._server_capabilities = resp.get("capabilities", {}) or {}

        negotiated = resp.get("protocolVersion") or PROTOCOL_VERSION
        if negotiated not in SUPPORTED_PROTOCOL_VERSIONS:
            raise ConnectionError(
                f"MCP server '{self.name}' negotiated protocol version "
                f"'{negotiated}', which this client does not implement "
                f"(supported: {', '.join(sorted(SUPPORTED_PROTOCOL_VERSIONS))})"
            )
        self._protocol_version = negotiated
        # Streamable HTTP requires this header on every subsequent request once a
        # version has been negotiated.
        if hasattr(self._transport, "set_protocol_version"):
            self._transport.set_protocol_version(negotiated)

        await self._notify("notifications/initialized", {})
        logger.info(
            "MCP server '{}' initialized: {} (protocol {})",
            self.name, self._server_info.get("name", "unknown"), negotiated,
        )
        return resp

    async def _list_paginated(
        self, method: str, result_key: str, timeout: float = 30,
    ) -> list[dict[str, Any]]:
        """Drain a cursor-paginated MCP list method.

        Every ``*/list`` method in MCP is cursor-paginated. Reading only the first
        page (what this client did before) silently truncated the tool set of any
        server that paginates, and silence is the problem: the agent cannot tell
        a server with 12 tools from one with 400 whose page size is 12.
        """
        items: list[dict[str, Any]] = []
        cursor: str | None = None

        for page in range(_MAX_LIST_PAGES):
            params: dict[str, Any] = {"cursor": cursor} if cursor else {}
            resp = await self._request(method, params, timeout=timeout)

            batch = resp.get(result_key, [])
            if isinstance(batch, list):
                items.extend(entry for entry in batch if isinstance(entry, dict))

            cursor = resp.get("nextCursor")
            if not cursor or not isinstance(cursor, str):
                return items
            if page == _MAX_LIST_PAGES - 1:
                logger.warning(
                    "MCP server '{}' still returned a cursor for {} after {} pages — "
                    "stopping with {} entries collected",
                    self.name, method, _MAX_LIST_PAGES, len(items),
                )
        return items

    async def list_tools(self, timeout: float = 30) -> list[dict[str, Any]]:
        return await self._list_paginated("tools/list", "tools", timeout=timeout)

    async def call_tool(
        self, name: str, arguments: dict[str, Any] | None = None, timeout: float = 120,
    ) -> dict[str, Any]:
        return await self._request(
            "tools/call", {"name": name, "arguments": arguments or {}}, timeout=timeout,
        )

    async def list_resources(self, timeout: float = 30) -> list[dict[str, Any]]:
        return await self._list_paginated("resources/list", "resources", timeout=timeout)

    async def read_resource(self, uri: str, timeout: float = 30) -> dict[str, Any]:
        return await self._request("resources/read", {"uri": uri}, timeout=timeout)

    async def list_resource_templates(self, timeout: float = 30) -> list[dict[str, Any]]:
        return await self._list_paginated(
            "resources/templates/list", "resourceTemplates", timeout=timeout,
        )

    async def list_prompts(self, timeout: float = 30) -> list[dict[str, Any]]:
        return await self._list_paginated("prompts/list", "prompts", timeout=timeout)

    async def get_prompt(
        self, name: str, arguments: dict[str, Any] | None = None, timeout: float = 30,
    ) -> dict[str, Any]:
        return await self._request(
            "prompts/get", {"name": name, "arguments": arguments or {}}, timeout=timeout,
        )

    async def _request(self, method: str, params: dict[str, Any], timeout: float = 30) -> dict[str, Any]:
        if self._closed:
            raise ConnectionError(f"MCP client '{self.name}' is closed")
        # Checked before the write so a request against a server whose reader has
        # already died fails immediately instead of after the full timeout.
        if not self._reader_alive:
            raise ConnectionError(
                f"MCP client '{self.name}' has no live reader — connection is down"
            )

        self._request_id += 1
        req_id = self._request_id
        message = {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}

        future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self._pending[req_id] = future

        try:
            try:
                await self._transport.send(message)
                return await asyncio.wait_for(future, timeout=timeout)
            except asyncio.TimeoutError:
                # Tell the server to stop working on a request nobody is waiting
                # for any more. Without this the server keeps burning resources
                # on (and may still bill for) a call we have abandoned.
                await self._cancel_request(req_id, "timeout")
                raise TimeoutError(f"MCP request '{method}' timed out after {timeout}s")
            except Exception as e:
                logger.warning("MCP request '{}' failed: {}", method, e)
                raise
        finally:
            # Always release the pending entry — covers normal return,
            # timeout, transport error, and CancelledError (which would
            # otherwise leak the future).
            pending = self._pending.pop(req_id, None)
            if pending is not None and not pending.done():
                pending.cancel()

    async def _cancel_request(self, req_id: int, reason: str) -> None:
        try:
            await self._notify(
                "notifications/cancelled", {"requestId": req_id, "reason": reason},
            )
        except Exception as e:
            # Best effort: the connection may already be gone, which is often
            # exactly why we timed out.
            logger.debug("Could not send cancellation for request {}: {}", req_id, e)

    async def _notify(self, method: str, params: dict[str, Any]) -> None:
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        await self._transport.send(message)

    async def _read_loop(self) -> None:
        """Dispatch inbound messages until the transport closes.

        The failure mode this is built around: the previous version wrapped the
        whole ``while`` in one ``except Exception`` that *returned*, so a single
        malformed frame terminated the reader for good. It did not take a hostile
        server — a JSON-RPC error whose ``error`` member was not an object made
        ``err.get(...)`` raise ``AttributeError`` — and because ``is_connected``
        only consulted the transport, nothing noticed. Every later call then hung
        for its full timeout (120s by default) until the process restarted.

        So: per-message errors are contained around the *dispatch of that
        message* and the loop continues; only the transport signalling closure
        ends it; and the exit path always fails the pending futures so callers
        learn immediately instead of by timeout.
        """
        try:
            while self._transport.is_connected and not self._closed:
                try:
                    msg = await self._transport.receive()
                except (ConnectionError, OSError) as e:
                    logger.warning("MCP server '{}' disconnected: {}", self.name, e)
                    break

                try:
                    await self._dispatch_message(msg)
                except Exception as e:
                    # Containment boundary. One bad frame is a bad frame, not the
                    # end of the connection.
                    logger.warning(
                        "MCP server '{}' sent a message this client could not process "
                        "({}): {}", self.name, e, str(msg)[:200],
                    )
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.error("MCP read loop error for '{}': {}", self.name, e)
        finally:
            self._reader_alive = False
            self._fail_pending(
                ConnectionError(f"MCP server '{self.name}' connection closed")
            )

    async def _dispatch_message(self, msg: Any) -> None:
        if not isinstance(msg, dict):
            raise ValueError(f"expected a JSON-RPC object, got {type(msg).__name__}")

        has_id = "id" in msg and msg["id"] is not None
        method = msg.get("method")

        if has_id and method:
            await self._handle_server_request(msg)
            return

        if has_id and "result" in msg:
            fut = self._pending.pop(msg["id"], None)
            if fut and not fut.done():
                result = msg["result"]
                fut.set_result(result if isinstance(result, dict) else {})
            return

        if has_id and "error" in msg:
            fut = self._pending.pop(msg["id"], None)
            if fut and not fut.done():
                fut.set_exception(RuntimeError(self._format_error(msg["error"])))
            return

        if method:
            await self._handle_notification(msg)
            return

        raise ValueError("message has neither method, result nor error")

    @staticmethod
    def _format_error(err: Any) -> str:
        """Render a JSON-RPC error member defensively.

        ``err`` is whatever the server sent. Assuming it was a dict here is the
        exact line that used to kill the reader.
        """
        if isinstance(err, dict):
            return f"MCP error {err.get('code', -1)}: {err.get('message', '')}"
        return f"MCP error: {str(err)[:200]}"

    async def _handle_server_request(self, msg: dict[str, Any]) -> None:
        """Answer a server→client request.

        We advertise no client capabilities, so every such request is for
        something we did not offer — except ``ping``, which is part of the base
        protocol and must be answered. Anything else gets a proper
        ``-32601 Method not found`` rather than silence: a server that is waiting
        on a reply otherwise stalls or drops the connection, and "no reply" is
        indistinguishable from "we crashed".
        """
        method = msg.get("method", "")
        req_id = msg.get("id")

        if method == "ping":
            await self._respond(req_id, {})
            return

        logger.debug(
            "MCP server '{}' requested '{}', which this client does not implement",
            self.name, method,
        )
        await self._respond_error(
            req_id, -32601, f"Method '{method}' is not supported by this client",
        )

    async def _respond(self, req_id: Any, result: dict[str, Any]) -> None:
        await self._transport.send({"jsonrpc": "2.0", "id": req_id, "result": result})

    async def _respond_error(self, req_id: Any, code: int, message: str) -> None:
        await self._transport.send(
            {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
        )

    async def _handle_notification(self, msg: dict[str, Any]) -> None:
        method = msg.get("method", "")
        params = msg.get("params") or {}

        if self._notification_handler is not None:
            try:
                await self._notification_handler(method, params if isinstance(params, dict) else {})
            except Exception as e:
                logger.warning("MCP notification handler failed for '{}': {}", method, e)

        # Bounded queue: if no consumer is draining, drop oldest
        # to keep memory steady on long-running connections.
        while True:
            try:
                self._notifications.put_nowait(msg)
                return
            except asyncio.QueueFull:
                try:
                    self._notifications.get_nowait()
                except asyncio.QueueEmpty:
                    return
