"""MCP transports — stdio and Streamable HTTP for JSON-RPC communication.

Two transports, both speaking the framing that MCP 2025-06-18 specifies:

* :class:`StdioTransport` — newline-delimited JSON over a child process's
  stdin/stdout.
* :class:`StreamableHttpTransport` — POST for client→server, with responses
  arriving either as a direct JSON body or as an SSE stream, plus an optional
  long-lived GET stream for server→client traffic.

The legacy ``HttpTransport`` (an HTTP+SSE client for the pre-2025-03-26
revision) used to live here. It was never constructed by the manager — only by
its own tests — and it carried the same framing bugs as the code below, so
keeping it meant maintaining two transports to fix one. It is gone; the
Streamable HTTP transport is the only HTTP path.
"""

from __future__ import annotations

import asyncio
import json
import re
from abc import ABC, abstractmethod
from typing import Any

import aiohttp

from loguru import logger

from echo_agent.agent.proc_lifecycle import spawn_exec, terminate_tree

#: Ceiling on a partially-received SSE event. A server that opens a stream and
#: never sends a blank line would otherwise grow this buffer without bound.
_MAX_SSE_BUFFER_BYTES = 8 * 1024 * 1024

#: Ceiling on a single JSON-RPC line from a stdio server, for the same reason.
_MAX_STDIO_LINE_BYTES = 32 * 1024 * 1024

#: Matches a CLI flag naming a secret, so the *following* argv element can be
#: masked before the command line reaches a log sink.
_SENSITIVE_FLAG_RE = re.compile(
    r"^--?(?:[\w-]*(?:key|token|secret|password|passwd|pwd|credential|auth)[\w-]*)$",
    re.IGNORECASE,
)

class MCPUnauthorizedError(ConnectionError):
    """The MCP server rejected our credentials (HTTP 401/403).

    Distinct from a generic transport failure because the remedy is different: a
    token can be revoked or invalidated long before its nominal expiry, and the
    only way to recover is to refresh it. Reported as a plain ConnectionError,
    the 401 was indistinguishable from "the network broke", so the stored token
    stayed in place and every reconnect replayed the same dead credential.

    Carries the ``WWW-Authenticate`` header, which is where the spec puts the
    ``resource_metadata`` URL that drives OAuth discovery.
    """

    def __init__(self, message: str, *, www_authenticate: str = "") -> None:
        super().__init__(message)
        self.www_authenticate = www_authenticate


#: Matches ``NAME=value`` / ``"token": "value"`` shaped secrets in free text,
#: used to scrub a child process's stderr before logging it.
_SECRET_ASSIGNMENT_RE = re.compile(
    r"((?:api[_-]?key|access[_-]?token|refresh[_-]?token|bearer|token|secret|password|credential)"
    r"\s*[=:]\s*[\"']?)([^\s\"',;]{6,})",
    re.IGNORECASE,
)


def redact_argv(command: str, args: list[str]) -> str:
    """Render a command line with secret-valued flags masked.

    The stdio command line was logged verbatim at debug level, and MCP servers
    are routinely configured as ``["--api-key", "sk-..."]`` — so enabling debug
    logging wrote third-party credentials to disk. Mirrors the argv masking the
    tool registry already applies to audited tool calls.
    """
    rendered: list[str] = [command]
    mask_next = False
    for arg in args:
        if mask_next:
            rendered.append("***")
            mask_next = False
            continue
        if "=" in arg and _SENSITIVE_FLAG_RE.match(arg.split("=", 1)[0]):
            rendered.append(arg.split("=", 1)[0] + "=***")
            continue
        mask_next = bool(_SENSITIVE_FLAG_RE.match(arg))
        rendered.append(arg)
    return " ".join(rendered)


def redact_text(text: str) -> str:
    """Mask secret-shaped assignments in free text (a server's stderr)."""
    return _SECRET_ASSIGNMENT_RE.sub(lambda m: m.group(1) + "***", text)


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

    def set_protocol_version(self, version: str) -> None:
        """Record the negotiated protocol revision. No-op for framings that do
        not carry it on the wire (stdio); overridden for Streamable HTTP, where
        the spec requires an ``MCP-Protocol-Version`` header on every request
        after the handshake."""
        return None


class StdioTransport(MCPTransport):

    def __init__(self, command: str, args: list[str] | None = None, env: dict[str, str] | None = None):
        self._command = command
        self._args = args or []
        self._env = env
        self._process: asyncio.subprocess.Process | None = None
        self._stderr_task: asyncio.Task | None = None
        self._send_lock = asyncio.Lock()

    async def connect(self, timeout: float = 60) -> None:
        import os
        merged_env = {**os.environ, **(self._env or {})}
        # spawn_exec, not create_subprocess_exec: it puts the server in its own
        # process group so `close()` can reclaim the whole tree. An MCP server is
        # very often a launcher (`npx`, `uvx`, a shell wrapper) whose real work
        # runs in a grandchild, and signalling only the direct child left that
        # grandchild running — an orphan holding the port or the credential it was
        # handed. Same infrastructure the skills executor already uses.
        self._process = await asyncio.wait_for(
            spawn_exec(
                self._command, *self._args,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                env=merged_env,
                # A stdout line longer than the default 64 KiB limit would raise
                # LimitOverrunError inside readline() — reported as a protocol
                # error on a server whose only sin was a large tool result.
                limit=_MAX_STDIO_LINE_BYTES,
            ),
            timeout=timeout,
        )
        # Drain stderr in the background so the child process never blocks on a full pipe buffer.
        self._stderr_task = asyncio.create_task(self._drain_stderr())
        logger.debug("Stdio transport started: {}", redact_argv(self._command, self._args))

    async def _drain_stderr(self) -> None:
        if not self._process or not self._process.stderr:
            return
        try:
            while True:
                try:
                    line = await self._process.stderr.readline()
                except (ValueError, asyncio.LimitOverrunError):
                    # Overlong stderr line: drop it rather than ending the drain,
                    # which would let the child block on a full pipe.
                    continue
                if not line:
                    return
                text = line.decode(errors="replace").rstrip()
                if text:
                    # Server logs frequently echo their own configuration,
                    # including the credentials they were handed.
                    logger.debug("[mcp-stderr {}] {}", self._command, redact_text(text[:500]))
        except asyncio.CancelledError:
            return
        except Exception as e:
            logger.debug("MCP stderr drain ended: {}", e)

    async def send(self, message: dict[str, Any]) -> None:
        if not self._process or not self._process.stdin:
            raise ConnectionError("Stdio transport not connected")
        if self._process.returncode is not None:
            raise ConnectionError(
                f"MCP server process exited with code {self._process.returncode}"
            )
        line = json.dumps(message, ensure_ascii=False) + "\n"
        # Serialize writes so a future caller using asyncio.gather can't
        # interleave half-encoded JSON-RPC frames on stdin.
        async with self._send_lock:
            try:
                self._process.stdin.write(line.encode())
                await self._process.stdin.drain()
            except (BrokenPipeError, ConnectionResetError) as e:
                # The child died between the check above and this write. Surface
                # it as ConnectionError so the read loop and callers treat it as
                # a disconnect rather than an unexpected internal fault.
                raise ConnectionError(f"MCP server closed its stdin: {e}") from e

    async def receive(self) -> dict[str, Any]:
        if not self._process or not self._process.stdout:
            raise ConnectionError("Stdio transport not connected")
        while True:
            try:
                line = await self._process.stdout.readline()
            except (ValueError, asyncio.LimitOverrunError) as e:
                raise ConnectionError(f"MCP server sent an oversized line: {e}") from e
            if not line:
                raise ConnectionError("Stdio transport closed")
            text = line.decode(errors="replace").strip()
            if not text:
                continue
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                # Servers that print banners or warnings to stdout are common;
                # skipping the line is right, ending the connection is not.
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

        process, self._process = self._process, None
        if process is None:
            return
        if process.stdin:
            try:
                process.stdin.close()
            except Exception as e:
                logger.debug("Failed to close MCP stdin: {}", e)
        # No early return on an already-exited process: a launcher that has
        # already exited can still have left the real server running in its
        # group, and `returncode is not None` was exactly the shortcut that let
        # those grandchildren escape. terminate_tree is a no-op on an empty
        # group, so this is cheap on the common path.
        try:
            await terminate_tree(process, grace=5.0)
        except ProcessLookupError:
            return
        except Exception as e:
            logger.warning(
                "MCP server '{}' could not be fully terminated: {}", self._command, e,
            )

    @property
    def is_connected(self) -> bool:
        return self._process is not None and self._process.returncode is None


class StreamableHttpTransport(MCPTransport):
    """MCP Streamable HTTP transport (2025-06-18).

    What the previous implementation got wrong, all of it observable against the
    official Python SDK:

    * **No ``Accept`` header.** The spec requires every POST to send
      ``Accept: application/json, text/event-stream``. Without it a compliant
      server answers 406.
    * **No HTTP status check.** That 406's JSON-RPC error body was pushed onto
      the response queue with an id the client had never issued, so no pending
      future matched and the call ran to its full timeout. A protocol error
      surfaced as a hang.
    * **LF-only SSE framing.** Events were split on ``\\n\\n``, but SSE permits
      ``\\r\\n\\r\\n`` and the official SDK emits it — so the parser never
      produced a single event against a real server.
    * **No ``MCP-Protocol-Version``**, no GET stream for server-initiated
      traffic, no session re-initialisation after a 404, no DELETE on shutdown.

    Each of those is fixed below. One deliberate design note: POSTs are no longer
    serialised behind a single lock. The lock existed to protect session-id and
    queue ordering, but it also serialised every concurrent tool call on a
    JSON-responding server. Ordering is instead protected where it belongs — the
    session id is assigned under a narrow lock, and the response queue is
    inherently order-independent because JSON-RPC correlates by request id.
    """

    def __init__(self, url: str, headers: dict[str, str] | None = None):
        self._url = url
        self._headers = headers or {}
        self._session: aiohttp.ClientSession | None = None
        self._session_id: str | None = None
        self._protocol_version: str | None = None
        self._response_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._connected = False
        self._sse_tasks: set[asyncio.Task] = set()
        self._listen_task: asyncio.Task | None = None
        # Guards session-id assignment only — not the request itself, so
        # concurrent tool calls stay concurrent.
        self._session_lock = asyncio.Lock()
        # Set when the server invalidates our session (404). The client is
        # expected to start a fresh session by re-initialising.
        self._session_expired = False
        # Set when the server answered 401/403, so a rebuild can refresh the
        # token instead of reconnecting with the same rejected one.
        self._auth_failed = False
        self._call_timeout: float | None = None

    @property
    def is_connected(self) -> bool:
        return self._connected

    @property
    def session_id(self) -> str | None:
        return self._session_id

    @property
    def session_expired(self) -> bool:
        return self._session_expired

    @property
    def auth_failed(self) -> bool:
        """Whether this server rejected our credentials.

        Read by the manager's supervisor to decide whether a rebuild needs a
        refreshed token or merely a new socket.
        """
        return self._auth_failed

    def set_protocol_version(self, version: str) -> None:
        self._protocol_version = version

    async def connect(self, timeout: float = 60) -> None:
        # `sock_connect` bounds establishing the TCP connection; `total` is left
        # unset on purpose. Using ClientTimeout(total=connect_timeout) meant the
        # 60s connect budget also capped every tools/call, so a server config of
        # timeout=120 could never exceed 60 — two settings in direct
        # contradiction, with the smaller silently winning — and it killed
        # long-lived SSE streams by design.
        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(sock_connect=timeout, connect=timeout),
        )
        self._connected = True
        self._session_expired = False
        logger.debug("Streamable HTTP transport connected to {}", self._url)

    def _request_headers(self, *, for_get: bool = False) -> dict[str, str]:
        headers = {
            **self._headers,
            # Required by the spec on every POST: the server chooses between a
            # JSON body and an SSE stream, so the client must accept both.
            "Accept": "text/event-stream" if for_get else "application/json, text/event-stream",
        }
        if not for_get:
            headers["Content-Type"] = "application/json"
        if self._session_id:
            headers["Mcp-Session-Id"] = self._session_id
        if self._protocol_version:
            headers["MCP-Protocol-Version"] = self._protocol_version
        return headers

    def set_call_timeout(self, timeout: float | None) -> None:
        """Bound how long a single POST may take, in seconds.

        Separate from the connect budget so the two can differ, which is the
        whole point of having both settings.
        """
        self._call_timeout = timeout

    async def send(self, message: dict[str, Any]) -> None:
        if not self._session:
            raise ConnectionError("Transport not connected")
        if self._session_expired:
            raise ConnectionError(
                "MCP session was invalidated by the server and must be re-initialised"
            )

        request_timeout = (
            aiohttp.ClientTimeout(total=self._call_timeout) if self._call_timeout else None
        )
        kwargs: dict[str, Any] = {
            "json": message,
            "headers": self._request_headers(),
        }
        if request_timeout is not None:
            kwargs["timeout"] = request_timeout

        resp_cm = self._session.post(self._url, **kwargs)
        resp = await resp_cm.__aenter__()
        release_required = True
        try:
            await self._capture_session_id(resp)

            if not await self._check_status(resp, message):
                return

            # 202 Accepted: the server took the message and will answer (if at
            # all) over a stream. Nothing to read from this body.
            if resp.status == 202:
                return

            content_type = resp.headers.get("Content-Type", "")

            if "text/event-stream" in content_type:
                # Stream the SSE payload in the background so send() returns
                # as soon as the response headers are in. Hand ownership of
                # the response context manager to the worker task.
                task = asyncio.create_task(self._consume_sse_response(resp_cm, resp))
                self._sse_tasks.add(task)
                task.add_done_callback(self._sse_tasks.discard)
                release_required = False
                return

            if "application/json" in content_type:
                await self._queue_json_body(resp)
                return

            # Unknown content type: try JSON before giving up, since some
            # servers answer with a bare or mislabelled body.
            try:
                await self._queue_json_body(resp)
            except Exception:
                text = await resp.text()
                logger.warning(
                    "Unexpected response type '{}': {}", content_type, text[:200],
                )
        finally:
            if release_required:
                await resp_cm.__aexit__(None, None, None)

    async def _capture_session_id(self, resp: Any) -> None:
        # Header names are case-insensitive in aiohttp's CIMultiDict, so this
        # matches Mcp-Session-Id in whatever casing the server chose.
        new_id = resp.headers.get("Mcp-Session-Id")
        if not new_id or new_id == self._session_id:
            return
        async with self._session_lock:
            self._session_id = new_id

    async def _check_status(self, resp: Any, message: dict[str, Any]) -> bool:
        """Raise on an HTTP-level failure. Returns False when there is no body
        worth reading.

        The old code never looked at the status at all, which is what turned a
        406 into a silent timeout.
        """
        if resp.status < 400:
            return True

        body = ""
        try:
            body = (await resp.text())[:300]
        except Exception:  # pragma: no cover - body already consumed/aborted
            pass

        if resp.status == 404 and self._session_id:
            # Session expired. Per spec the client starts a new session by
            # re-initialising; flagging it lets the manager rebuild rather than
            # reporting a generic HTTP error forever.
            self._session_expired = True
            self._session_id = None
            # Marked not-connected so the supervisor actually notices. Recording
            # the condition without this left `is_connected` True, so nothing
            # rebuilt and every later call failed on the stale session instead.
            self._connected = False
            raise ConnectionError(
                f"MCP session expired (HTTP 404): {body}"
            )

        if resp.status in (401, 403):
            # Recorded as well as raised: the raise reaches whoever made this
            # call, but recovery happens in the manager's supervisor, which only
            # sees "the connection is down". Without this flag it would rebuild
            # the socket and replay the same dead credential.
            self._auth_failed = True
            # Same reasoning as the 404 above: the supervisor only acts on a
            # connection that reports itself down.
            self._connected = False
            raise MCPUnauthorizedError(
                f"MCP server rejected the request as unauthorized (HTTP {resp.status}): {body}",
                www_authenticate=resp.headers.get("WWW-Authenticate", ""),
            )

        method = message.get("method", "?")
        raise ConnectionError(
            f"MCP HTTP error {resp.status} for '{method}': {body}"
        )

    async def _queue_json_body(self, resp: Any) -> None:
        data = await resp.json(content_type=None)
        # A JSON-RPC batch response is a list; each element correlates
        # independently by id.
        if isinstance(data, list):
            for entry in data:
                if isinstance(entry, dict):
                    await self._response_queue.put(entry)
        elif isinstance(data, dict):
            await self._response_queue.put(data)

    async def open_notification_stream(self) -> None:
        """Open the long-lived GET stream for server→client messages.

        Without this the only inbound path is the body of a POST response, so a
        server-initiated request or notification (``tools/list_changed`` in
        particular) had no way to reach us at all. Servers are allowed to refuse
        the GET with 405, which is not an error — it just means this server has
        nothing to push.
        """
        if self._listen_task and not self._listen_task.done():
            return
        self._listen_task = asyncio.create_task(self._listen_get_stream())

    async def _listen_get_stream(self) -> None:
        if not self._session:
            return
        try:
            async with self._session.get(
                self._url,
                headers=self._request_headers(for_get=True),
                # No total budget: this stream is meant to stay open.
                timeout=aiohttp.ClientTimeout(sock_connect=30),
            ) as resp:
                if resp.status == 405:
                    logger.debug(
                        "MCP server {} does not offer a GET notification stream", self._url,
                    )
                    return
                if resp.status >= 400:
                    logger.warning(
                        "MCP notification stream rejected with HTTP {}", resp.status,
                    )
                    return
                await self._capture_session_id(resp)
                await self._read_sse_response(resp)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            if self._connected:
                logger.debug("MCP notification stream ended: {}", e)

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
            except Exception as e:
                logger.debug("Failed to close streamable HTTP response: {}", e)

    async def _read_sse_response(self, resp: Any) -> None:
        """Parse an SSE byte stream into JSON-RPC messages.

        Framing follows the SSE specification rather than a guess at it: events
        are separated by a blank line in any of the three legal forms
        (``\\r\\n\\r\\n``, ``\\n\\n``, ``\\r\\r``), and a ``data:`` field's value
        drops exactly one optional leading space. The previous parser split only
        on ``\\n\\n`` and required the literal prefix ``"data: "`` with a space,
        so a CRLF stream — what the official SDK sends — never yielded one event
        and every call over SSE timed out.
        """
        buffer = ""
        async for chunk in resp.content.iter_any():
            buffer += chunk.decode("utf-8", errors="replace")

            while True:
                event_text, remainder = _split_sse_event(buffer)
                if event_text is None:
                    break
                buffer = remainder
                message = _parse_sse_event(event_text)
                if message is not None:
                    await self._response_queue.put(message)

            if len(buffer) > _MAX_SSE_BUFFER_BYTES:
                raise ConnectionError(
                    "MCP SSE stream exceeded the buffer limit without a complete event"
                )

        # Stream ended. A trailing event without its terminating blank line is
        # still a complete event per spec.
        if buffer.strip():
            message = _parse_sse_event(buffer)
            if message is not None:
                await self._response_queue.put(message)

    async def receive(self) -> dict[str, Any]:
        return await self._response_queue.get()

    async def close(self) -> None:
        self._connected = False

        if self._listen_task and not self._listen_task.done():
            self._listen_task.cancel()
            try:
                await self._listen_task
            except (asyncio.CancelledError, Exception):
                pass
        self._listen_task = None

        for task in list(self._sse_tasks):
            if not task.done():
                task.cancel()
        if self._sse_tasks:
            await asyncio.gather(*self._sse_tasks, return_exceptions=True)
            self._sse_tasks.clear()

        # Tell the server the session is over so it can release state, instead of
        # leaving it to time out. Best effort: we are shutting down either way.
        if self._session and self._session_id and not self._session_expired:
            try:
                async with self._session.delete(
                    self._url,
                    headers=self._request_headers(),
                    timeout=aiohttp.ClientTimeout(total=5),
                ) as resp:
                    logger.debug("MCP session DELETE returned {}", resp.status)
            except Exception as e:
                logger.debug("Failed to terminate MCP session: {}", e)

        self._session_id = None
        if self._session:
            await self._session.close()
            self._session = None
        logger.debug("Streamable HTTP transport closed")


def _split_sse_event(buffer: str) -> tuple[str | None, str]:
    """Split the first complete SSE event off *buffer*.

    Returns ``(event_text, remainder)``, with ``event_text`` None when no
    terminator has arrived yet. All three legal blank-line forms are recognised,
    and the earliest one wins so a stream mixing them cannot desynchronise.
    """
    best_index: int | None = None
    best_len = 0
    for terminator in ("\r\n\r\n", "\n\n", "\r\r"):
        index = buffer.find(terminator)
        if index == -1:
            continue
        if best_index is None or index < best_index or (
            index == best_index and len(terminator) > best_len
        ):
            best_index = index
            best_len = len(terminator)

    if best_index is None:
        return None, buffer
    return buffer[:best_index], buffer[best_index + best_len:]


def _parse_sse_event(event_text: str) -> dict[str, Any] | None:
    """Extract the JSON-RPC message from one SSE event block."""
    data_lines: list[str] = []
    for line in event_text.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        if line.startswith(":"):
            continue  # comment / keep-alive
        if not line.startswith("data"):
            continue
        remainder = line[4:]
        if remainder.startswith(":"):
            remainder = remainder[1:]
            # Spec: strip a single leading space, not all whitespace — JSON
            # tolerates it either way, but indented payloads must survive.
            if remainder.startswith(" "):
                remainder = remainder[1:]
        elif remainder:
            continue  # a field like "dataFoo", not "data"
        data_lines.append(remainder)

    if not data_lines:
        return None

    raw = "\n".join(data_lines).strip()
    if not raw:
        return None
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        logger.warning("Non-JSON SSE data: {}", raw[:200])
        return None
    return parsed if isinstance(parsed, dict) else None
