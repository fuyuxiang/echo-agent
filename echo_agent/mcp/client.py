"""MCP client — JSON-RPC 2.0 communication with a single MCP server."""

from __future__ import annotations

import asyncio
import json
from typing import Any

from loguru import logger

from echo_agent.mcp.transport import MCPTransport


class MCPClient:

    def __init__(self, name: str, transport: MCPTransport):
        self.name = name
        self._transport = transport
        self._request_id = 0
        self._pending: dict[int, asyncio.Future] = {}
        self._notifications: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._reader_task: asyncio.Task | None = None
        self._server_info: dict[str, Any] = {}
        self._server_capabilities: dict[str, Any] = {}

    async def connect(self, timeout: float = 60) -> None:
        if hasattr(self._transport, "connect"):
            await self._transport.connect(timeout=timeout)
        self._reader_task = asyncio.create_task(self._read_loop())
        await self.initialize()

    async def disconnect(self) -> None:
        if self._reader_task:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except (asyncio.CancelledError, Exception):
                pass
        await self._transport.close()
        for fut in self._pending.values():
            if not fut.done():
                fut.cancel()
        self._pending.clear()

    @property
    def is_connected(self) -> bool:
        return self._transport.is_connected

    async def initialize(self) -> dict[str, Any]:
        resp = await self._request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {"sampling": {}},
            "clientInfo": {"name": "echo-agent", "version": "0.1.0"},
        })
        self._server_info = resp.get("serverInfo", {})
        self._server_capabilities = resp.get("capabilities", {})
        await self._notify("notifications/initialized", {})
        logger.info("MCP server '{}' initialized: {}", self.name, self._server_info.get("name", "unknown"))
        return resp

    async def list_tools(self) -> list[dict[str, Any]]:
        resp = await self._request("tools/list", {})
        return resp.get("tools", [])

    async def call_tool(self, name: str, arguments: dict[str, Any] | None = None, timeout: float = 120) -> dict[str, Any]:
        resp = await self._request("tools/call", {"name": name, "arguments": arguments or {}}, timeout=timeout)
        return resp

    async def list_resources(self) -> list[dict[str, Any]]:
        resp = await self._request("resources/list", {})
        return resp.get("resources", [])

    async def read_resource(self, uri: str) -> dict[str, Any]:
        resp = await self._request("resources/read", {"uri": uri})
        return resp

    async def list_prompts(self) -> list[dict[str, Any]]:
        resp = await self._request("prompts/list", {})
        return resp.get("prompts", [])

    async def get_prompt(self, name: str, arguments: dict[str, Any] | None = None) -> dict[str, Any]:
        resp = await self._request("prompts/get", {"name": name, "arguments": arguments or {}})
        return resp

    async def _request(self, method: str, params: dict[str, Any], timeout: float = 30) -> dict[str, Any]:
        self._request_id += 1
        req_id = self._request_id
        message = {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}

        future: asyncio.Future[dict[str, Any]] = asyncio.get_event_loop().create_future()
        self._pending[req_id] = future

        try:
            await self._transport.send(message)
            result = await asyncio.wait_for(future, timeout=timeout)
            return result
        except asyncio.TimeoutError:
            self._pending.pop(req_id, None)
            raise TimeoutError(f"MCP request '{method}' timed out after {timeout}s")
        except Exception:
            self._pending.pop(req_id, None)
            raise

    async def _notify(self, method: str, params: dict[str, Any]) -> None:
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        await self._transport.send(message)

    async def _read_loop(self) -> None:
        try:
            while self._transport.is_connected:
                try:
                    msg = await self._transport.receive()
                except ConnectionError:
                    logger.warning("MCP server '{}' disconnected", self.name)
                    break

                if "id" in msg and "result" in msg:
                    req_id = msg["id"]
                    fut = self._pending.pop(req_id, None)
                    if fut and not fut.done():
                        fut.set_result(msg["result"])
                elif "id" in msg and "error" in msg:
                    req_id = msg["id"]
                    fut = self._pending.pop(req_id, None)
                    err = msg["error"]
                    if fut and not fut.done():
                        fut.set_exception(RuntimeError(f"MCP error {err.get('code', -1)}: {err.get('message', '')}"))
                elif "method" in msg and "id" not in msg:
                    await self._notifications.put(msg)
                else:
                    logger.debug("Unhandled MCP message: {}", str(msg)[:200])
        except asyncio.CancelledError:
            return
        except Exception as e:
            logger.error("MCP read loop error for '{}': {}", self.name, e)
