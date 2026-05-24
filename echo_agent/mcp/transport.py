"""MCP transports — stdio and HTTP/SSE for JSON-RPC communication."""

from __future__ import annotations

import asyncio
import json
from abc import ABC, abstractmethod
from typing import Any

import aiohttp

from loguru import logger


class MCPTransport(ABC):

    @abstractmethod
    async def send(self, message: dict[str, Any]) -> None: ...

    @abstractmethod
    async def receive(self) -> dict[str, Any]: ...

    @abstractmethod
    async def close(self) -> None: ...

    @property
    @abstractmethod
    def is_connected(self) -> bool: ...


class StdioTransport(MCPTransport):

    def __init__(self, command: str, args: list[str] | None = None, env: dict[str, str] | None = None):
        self._command = command
        self._args = args or []
        self._env = env
        self._process: asyncio.subprocess.Process | None = None
        self._read_buffer = b""
        self._stderr_task: asyncio.Task | None = None
        self._send_lock = asyncio.Lock()

    async def connect(self, timeout: float = 60) -> None:
        import os
        merged_env = {**os.environ, **(self._env or {})}
        self._process = await asyncio.wait_for(
            asyncio.create_subprocess_exec(
                self._command, *self._args,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                env=merged_env,
            ),
            timeout=timeout,
        )
        # Drain stderr in the background so the child process never blocks on a full pipe buffer.
        self._stderr_task = asyncio.create_task(self._drain_stderr())
        logger.debug("Stdio transport started: {} {}", self._command, " ".join(self._args))

    async def _drain_stderr(self) -> None:
        if not self._process or not self._process.stderr:
            return
        try:
            while True:
                line = await self._process.stderr.readline()
                if not line:
                    return
                text = line.decode(errors="replace").rstrip()
                if text:
                    logger.debug("[mcp-stderr {}] {}", self._command, text[:500])
        except asyncio.CancelledError:
            return
        except Exception as e:
            logger.debug("MCP stderr drain ended: {}", e)

    async def send(self, message: dict[str, Any]) -> None:
        if not self._process or not self._process.stdin:
            raise ConnectionError("Stdio transport not connected")
        line = json.dumps(message, ensure_ascii=False) + "\n"
        # Serialize writes so a future caller using asyncio.gather can't
        # interleave half-encoded JSON-RPC frames on stdin.
        async with self._send_lock:
            self._process.stdin.write(line.encode())
            await self._process.stdin.drain()

    async def receive(self) -> dict[str, Any]:
        if not self._process or not self._process.stdout:
            raise ConnectionError("Stdio transport not connected")
        while True:
            line = await self._process.stdout.readline()
            if not line:
                raise ConnectionError("Stdio transport closed")
            text = line.decode().strip()
            if not text:
                continue
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                logger.warning("Non-JSON line from MCP server: {}", text[:200])
                continue

    async def close(self) -> None:
        if self._stderr_task and not self._stderr_task.done():
            self._stderr_task.cancel()
            try:
                await self._stderr_task
            except (asyncio.CancelledError, Exception):
                pass
            self._stderr_task = None
        if self._process:
            if self._process.stdin:
                self._process.stdin.close()
            self._process.terminate()
            try:
                await asyncio.wait_for(self._process.wait(), timeout=5)
            except asyncio.TimeoutError:
                self._process.kill()
                await self._process.wait()
            self._process = None

    @property
    def is_connected(self) -> bool:
        return self._process is not None and self._process.returncode is None


class HttpTransport(MCPTransport):

    def __init__(self, url: str, headers: dict[str, str] | None = None):
        self._url = url.rstrip("/")
        self._headers = headers or {}
        self._session: Any = None
        self._sse_response: Any = None
        self._sse_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._sse_task: asyncio.Task | None = None
        self._connected = False
        self._send_lock = asyncio.Lock()

    async def connect(self, timeout: float = 60) -> None:
        import aiohttp
        self._session = aiohttp.ClientSession(
            headers=self._headers,
            timeout=aiohttp.ClientTimeout(total=timeout),
        )
        self._sse_task = asyncio.create_task(self._listen_sse())
        self._connected = True
        logger.debug("HTTP transport connected: {}", self._url)

    async def send(self, message: dict[str, Any]) -> None:
        if not self._session:
            raise ConnectionError("HTTP transport not connected")
        async with self._send_lock:
            async with self._session.post(
                self._url,
                json=message,
                headers={"Content-Type": "application/json"},
            ) as resp:
                if resp.status >= 400:
                    body = await resp.text()
                    raise ConnectionError(f"MCP HTTP error {resp.status}: {body[:300]}")
                if resp.content_type == "application/json":
                    data = await resp.json()
                    await self._sse_queue.put(data)

    async def receive(self) -> dict[str, Any]:
        return await self._sse_queue.get()

    async def close(self) -> None:
        self._connected = False
        if self._sse_task:
            self._sse_task.cancel()
            try:
                await self._sse_task
            except (asyncio.CancelledError, Exception):
                pass
        if self._sse_response:
            self._sse_response.close()
        if self._session:
            await self._session.close()
            self._session = None

    @property
    def is_connected(self) -> bool:
        return self._connected

    async def _listen_sse(self) -> None:
        if not self._session:
            return
        try:
            sse_url = self._url
            if not sse_url.endswith("/sse"):
                sse_url = sse_url + "/sse"
            async with self._session.get(sse_url, headers={"Accept": "text/event-stream"}) as resp:
                self._sse_response = resp
                buffer = ""
                async for chunk in resp.content.iter_any():
                    buffer += chunk.decode(errors="replace")
                    while "\n\n" in buffer:
                        event_text, buffer = buffer.split("\n\n", 1)
                        data = self._parse_sse_event(event_text)
                        if data is not None:
                            await self._sse_queue.put(data)
        except asyncio.CancelledError:
            return
        except Exception as e:
            if self._connected:
                logger.warning("SSE connection lost: {}", e)
                self._connected = False

    def _parse_sse_event(self, text: str) -> dict[str, Any] | None:
        data_lines = []
        for line in text.splitlines():
            if line.startswith("data:"):
                data_lines.append(line[5:].strip())
        if not data_lines:
            return None
        try:
            return json.loads("\n".join(data_lines))
        except json.JSONDecodeError:
            return None


class StreamableHttpTransport(MCPTransport):
    """MCP Streamable HTTP transport (2025-03-26 spec).

    Replaces legacy SSE with bidirectional streamable HTTP:
    - POST sends JSON-RPC, response can be direct JSON or SSE stream
    - Mcp-Session-Id header for session management
    - Falls back to legacy HTTP if server doesn't support streamable
    """

    def __init__(self, url: str, headers: dict[str, str] | None = None):
        self._url = url
        self._headers = headers or {}
        self._session: aiohttp.ClientSession | None = None
        self._session_id: str | None = None
        self._response_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._connected = False
        self._sse_task: asyncio.Task | None = None
        self._sse_tasks: set[asyncio.Task] = set()
        self._send_lock = asyncio.Lock()

    @property
    def is_connected(self) -> bool:
        return self._connected

    @property
    def session_id(self) -> str | None:
        return self._session_id

    async def connect(self, timeout: float = 60) -> None:
        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=timeout)
        )
        self._connected = True
        logger.debug("Streamable HTTP transport connected to {}", self._url)

    async def send(self, message: dict[str, Any]) -> None:
        if not self._session:
            raise ConnectionError("Transport not connected")

        headers = {**self._headers, "Content-Type": "application/json"}
        if self._session_id:
            headers["Mcp-Session-Id"] = self._session_id

        # Serialize POSTs so we never have two in-flight requests racing on the
        # session_id / response_queue ordering invariants.
        async with self._send_lock:
            resp_cm = self._session.post(self._url, json=message, headers=headers)
            resp = await resp_cm.__aenter__()
            release_required = True
            try:
                if "Mcp-Session-Id" in resp.headers:
                    self._session_id = resp.headers["Mcp-Session-Id"]

                content_type = resp.headers.get("Content-Type", "")

                if "text/event-stream" in content_type:
                    # Stream the SSE payload in the background so send() returns
                    # as soon as the response headers are in. Hand ownership of
                    # the response context manager to the worker task.
                    task = asyncio.create_task(
                        self._consume_sse_response(resp_cm, resp)
                    )
                    self._sse_tasks.add(task)
                    task.add_done_callback(self._sse_tasks.discard)
                    release_required = False
                    return

                if "application/json" in content_type:
                    data = await resp.json()
                    await self._response_queue.put(data)
                    return

                try:
                    data = await resp.json()
                    await self._response_queue.put(data)
                except Exception:
                    text = await resp.text()
                    logger.warning("Unexpected response type '{}': {}", content_type, text[:200])
            finally:
                if release_required:
                    await resp_cm.__aexit__(None, None, None)

    async def _consume_sse_response(self, resp_cm: Any, resp: Any) -> None:
        try:
            await self._read_sse_response(resp)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            if self._connected:
                logger.warning("Streamable HTTP SSE consume failed: {}", e)
        finally:
            try:
                await resp_cm.__aexit__(None, None, None)
            except Exception:
                pass

    async def _read_sse_response(self, resp: Any) -> None:
        """Parse SSE events from streaming response."""
        buffer = ""
        async for chunk in resp.content:
            text = chunk.decode("utf-8", errors="replace")
            buffer += text
            while "\n\n" in buffer:
                event_text, buffer = buffer.split("\n\n", 1)
                data_lines = []
                for line in event_text.split("\n"):
                    if line.startswith("data: "):
                        data_lines.append(line[6:])
                if data_lines:
                    raw = "\n".join(data_lines)
                    try:
                        parsed = json.loads(raw)
                        await self._response_queue.put(parsed)
                    except json.JSONDecodeError:
                        logger.warning("Non-JSON SSE data: {}", raw[:200])

    async def receive(self) -> dict[str, Any]:
        return await self._response_queue.get()

    async def close(self) -> None:
        self._connected = False
        if self._sse_task and not self._sse_task.done():
            self._sse_task.cancel()
        for task in list(self._sse_tasks):
            if not task.done():
                task.cancel()
        if self._sse_tasks:
            await asyncio.gather(*self._sse_tasks, return_exceptions=True)
            self._sse_tasks.clear()
        if self._session:
            await self._session.close()
            self._session = None
        logger.debug("Streamable HTTP transport closed")
