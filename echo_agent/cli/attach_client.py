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
        raise NoGatewayError(f"{url} 无法连接。") from e


def diagnose_no_gateway(
    url: str, config_path: str | None, workspace: str | None
) -> str:
    """Turn a bare connection failure into an actionable message.

    The failure has three distinct causes and the fix differs for each, so we
    read the EXISTING config to tell them apart instead of emitting one generic
    "start the service" line (which misleads when the service is actually up
    but the gateway component is simply disabled)."""
    try:
        from echo_agent.config.loader import load_config, resolve_config_file
        cp = config_path
        if cp is None and workspace:
            cp = resolve_config_file(search_dir=workspace)
        cfg = load_config(config_path=cp)
        enabled = bool(cfg.gateway.enabled)
    except Exception:
        # (c) Config missing/unreadable — likely not set up yet.
        return (
            f"未发现本机常驻 echo-agent（{url} 无法连接），且读取配置失败。\n"
            "请先运行 echo-agent setup 完成配置，并确认 echo-agent 服务已启动。"
        )

    if not enabled:
        # (b) The exact trap: service can be running (channels work), but the
        # gateway component is off, so the CLI has nothing to attach to.
        return (
            f"未发现本机常驻 echo-agent（{url} 无法连接）。\n"
            "检测到配置中 gateway.enabled=false：网关组件未启用，"
            "因此 echo-agent cli 无法连接（微信/QQ 等频道不依赖网关，仍可正常工作）。\n"
            "修复：把配置里的 gateway.enabled 设为 true（host 保持 127.0.0.1），"
            "重启 echo-agent 服务后重试；或临时前台运行 echo-agent gateway。"
        )

    # (a) Gateway is enabled but unreachable — not started, crashed, or the
    # port is taken by another process.
    return (
        f"未发现本机常驻 echo-agent（{url} 无法连接）。\n"
        "配置中 gateway.enabled=true，但该端口无响应。请确认：\n"
        "  1. echo-agent 服务正在运行（systemctl is-active echo-agent）；\n"
        "  2. 端口未被其他进程占用；\n"
        "  3. 启动日志中出现 Gateway listening / ECHO_AGENT_READY。"
    )


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
    *, host: str, port: int, ws_path: str, user_id: str, token: str,
    config_path: str | None = None, workspace: str | None = None
) -> int:
    try:
        return asyncio.run(run_client(
            host=host, port=port, ws_path=ws_path,
            user_id=user_id, token=token,
        ))
    except NoGatewayError:
        url = build_ws_url(host, port, ws_path)
        print(diagnose_no_gateway(url, config_path, workspace))
        return 1
    except AuthError as e:
        print(f"认证失败：{e}")
        return 1
    except KeyboardInterrupt:
        return 0
