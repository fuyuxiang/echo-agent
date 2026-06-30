"""echo-agent cli — thin WebSocket client that attaches to a running
gateway on loopback and opens its own (cli:) session. Builds no agent."""

from __future__ import annotations

import asyncio
import sys
import threading

import aiohttp


class NoGatewayError(Exception):
    """Raised when no local gateway can be reached."""


def build_ws_url(host: str, port: int, ws_path: str) -> str:
    if not ws_path.startswith("/"):
        ws_path = "/" + ws_path
    return f"ws://{host}:{port}{ws_path}"


async def connect_ws(
    session: aiohttp.ClientSession, url: str
) -> aiohttp.ClientWebSocketResponse:
    try:
        return await session.ws_connect(url, heartbeat=30)
    except (aiohttp.ClientError, OSError) as e:
        raise NoGatewayError(
            f"未发现本机常驻 echo-agent（{url} 无法连接）。"
            f"请先用 systemd/launchd 启动，或运行 echo-agent gateway。"
        ) from e


class AuthError(Exception):
    """WS auth handshake rejected by the server."""


async def authenticate(
    ws, *, platform: str, user_id: str, session_key: str, token: str
) -> str:
    await ws.send_json({
        "type": "auth",
        "platform": platform,
        "user_id": user_id,
        "session_key": session_key,
        "token": token,
    })
    msg = await ws.receive_json()
    if msg.get("type") != "auth_ok":
        raise AuthError(msg.get("error") or "auth failed")
    return msg.get("session_key", session_key)


class OutboundRenderer:
    """Renders gateway outbound payloads to the terminal, mirroring the local
    CLIChannel stream/final de-dup logic (see channels/cli.py:_send_stream).

    Reads the real keys emitted by gateway/server.py:_build_outbound_payload:
    streaming flag lives in metadata._token_stream, the stream group id in
    metadata._inbound_event_id, and is_final is a top-level bool (or
    message_kind == "final")."""

    def __init__(self) -> None:
        self._stream_printed: dict[str, str] = {}
        self._max_entries = 32

    def render(self, payload: dict) -> None:
        text = payload.get("text") or ""
        meta = payload.get("metadata") or {}
        streaming = bool(meta.get("_token_stream"))
        if not streaming:
            if text:
                print(f"\n{text}\n")
            return

        eid = str(meta.get("_inbound_event_id", ""))
        is_final = payload.get("is_final", True) or payload.get("message_kind") == "final"
        if not is_final:
            if eid not in self._stream_printed:
                print()  # open the reply block
                self._stream_printed[eid] = ""
                while len(self._stream_printed) > self._max_entries:
                    self._stream_printed.pop(next(iter(self._stream_printed)))
            print(text, end="", flush=True)
            self._stream_printed[eid] += text
            return

        printed = self._stream_printed.pop(eid, "")
        if not printed:
            if text:
                print(f"\n{text}\n")
            return
        if text.startswith(printed):
            remainder = text[len(printed):]
            if remainder:
                print(remainder, end="", flush=True)
            print("\n")
        else:
            # Final text diverged from the streamed chunks — reprint cleanly.
            print(f"\n--- 完整回复 ---\n{text}\n")


async def _send_loop(ws) -> None:
    # stdin is read on a daemon thread so run_client teardown never has to
    # join a thread blocked in readline(): when _recv_loop finishes first and
    # this coroutine is cancelled, the daemon thread is simply abandoned and
    # dies with the process (asyncio.run's shutdown_default_executor would
    # otherwise hang forever joining a readline-blocked executor thread).
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue[str | None] = asyncio.Queue()
    stop_event = threading.Event()

    def read_stdin() -> None:
        while not stop_event.is_set():
            try:
                line = sys.stdin.readline()
            except (EOFError, KeyboardInterrupt):
                line = ""
            item = line if line != "" else None
            try:
                loop.call_soon_threadsafe(queue.put_nowait, item)
            except RuntimeError:
                break
            if item is None:  # EOF (Ctrl-D)
                break

    thread = threading.Thread(target=read_stdin, name="echo-agent-cli-attach-input", daemon=True)
    thread.start()
    try:
        while True:
            print("You> ", end="", flush=True)
            line = await queue.get()
            if line is None:  # EOF (Ctrl-D)
                break
            line = line.strip()
            if not line:
                continue
            if line.lower() in ("exit", "quit", "/quit"):
                break
            await ws.send_json({"type": "message", "text": line})
    finally:
        stop_event.set()


async def _recv_loop(ws, renderer: "OutboundRenderer") -> None:
    async for msg in ws:
        if msg.type != aiohttp.WSMsgType.TEXT:
            break
        try:
            payload = msg.json()
        except Exception:
            continue
        mtype = payload.get("type")
        if mtype == "error":
            print(f"\n[错误] {payload.get('error')}\n")
            continue
        if mtype in ("accepted", "auth_ok", "pong"):
            continue
        renderer.render(payload)


async def run_client(
    *, host: str, port: int, ws_path: str, user_id: str, token: str
) -> int:
    url = build_ws_url(host, port, ws_path)
    async with aiohttp.ClientSession() as session:
        ws = await connect_ws(session, url)
        await authenticate(
            ws, platform="cli", user_id=user_id,
            session_key=f"cli:{user_id}", token=token,
        )
        renderer = OutboundRenderer()
        send = asyncio.create_task(_send_loop(ws))
        recv = asyncio.create_task(_recv_loop(ws, renderer))
        done, pending = await asyncio.wait(
            {send, recv}, return_when=asyncio.FIRST_COMPLETED
        )
        for t in pending:
            t.cancel()
        await asyncio.gather(*pending, return_exceptions=True)
        await ws.close()
    return 0


def resolve_defaults(
    config_path: str | None, workspace: str | None
) -> tuple[str, int, str, str]:
    """Read connection defaults from the EXISTING gateway config (no new
    config fields). Host is pinned to loopback — cli is local-only."""
    try:
        from echo_agent.config.loader import load_config, resolve_config_file
        if config_path is None and workspace:
            config_path = resolve_config_file(search_dir=workspace)
        cfg = load_config(config_path=config_path)
        gw = cfg.gateway
        token = gw.auth.api_tokens[0] if gw.auth.api_tokens else ""
        return "127.0.0.1", int(gw.port), gw.ws_path, token
    except Exception:
        return "127.0.0.1", 9000, "/ws", ""


def run_cli_attach(
    *, host: str, port: int, ws_path: str, user_id: str, token: str
) -> int:
    try:
        return asyncio.run(run_client(
            host=host, port=port, ws_path=ws_path,
            user_id=user_id, token=token,
        ))
    except NoGatewayError as e:
        print(str(e))
        return 1
    except AuthError as e:
        print(f"认证失败：{e}")
        return 1
    except KeyboardInterrupt:
        return 0
