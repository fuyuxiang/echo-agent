"""echo-agent cli — thin WebSocket client that attaches to a running
gateway on loopback and opens its own (cli:) session. Builds no agent."""

from __future__ import annotations

import asyncio

import aiohttp


class NoGatewayError(Exception):
    """Raised when no local gateway can be reached."""


class MissingTUIDependencyError(Exception):
    """Raised when the optional TUI dependency (textual) is not installed.

    Kept distinct from NoGatewayError so run_cli_attach can surface the
    install hint directly instead of misdirecting the user to gateway/port
    troubleshooting (the gateway may be perfectly healthy)."""


def _require_textual() -> None:
    try:
        import textual  # noqa: F401
    except ImportError as e:
        raise MissingTUIDependencyError(
            "缺少 TUI 依赖 textual。请安装：pip install \"echo-agent[all]\" "
            "或 pip install \"echo-agent[tui]\"。"
        ) from e


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


async def run_client(
    *, host: str, port: int, ws_path: str, user_id: str, token: str
) -> int:
    _require_textual()
    from echo_agent.cli.tui.app import EchoTUI
    from echo_agent.cli.tui.bridge import WSBridge

    url = build_ws_url(host, port, ws_path)
    async with aiohttp.ClientSession() as session:
        ws = await connect_ws(session, url)
        session_key = await authenticate(
            ws, platform="cli", user_id=user_id,
            session_key=f"cli:{user_id}", token=token,
        )

        async def send_coro(text: str) -> None:
            # The gateway may have closed the socket without an error frame; a
            # send on a dead ws would raise. Swallow it and reflect the drop in
            # the status bar instead of crashing the input handler.
            try:
                await ws.send_json({"type": "message", "text": text})
            except (aiohttp.ClientError, ConnectionError, RuntimeError):
                app.notify_disconnected()

        app = EchoTUI(send_coro=send_coro, session_key=session_key)
        bridge = WSBridge(app)

        async def pump() -> None:
            # app.run_async() and this coroutine share one event loop, so
            # bridge.dispatch (which updates widgets) is called directly — no
            # call_from_thread needed.
            async for msg in ws:
                if msg.type != aiohttp.WSMsgType.TEXT:
                    break
                try:
                    payload = msg.json()
                except Exception:
                    continue
                bridge.dispatch(payload)
            # Loop exit means the socket closed / a non-TEXT frame arrived. No
            # error frame is sent on a clean gateway shutdown, so flip the
            # status bar to disconnected here rather than leaving it "●已连接".
            app.notify_disconnected()

        pump_task = asyncio.create_task(pump())
        try:
            await app.run_async()
        finally:
            pump_task.cancel()
            await asyncio.gather(pump_task, return_exceptions=True)
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
        return "127.0.0.1", 58123, "/ws", ""


def run_cli_attach(
    *, host: str, port: int, ws_path: str, user_id: str, token: str,
    config_path: str | None = None, workspace: str | None = None
) -> int:
    try:
        return asyncio.run(run_client(
            host=host, port=port, ws_path=ws_path,
            user_id=user_id, token=token,
        ))
    except MissingTUIDependencyError as e:
        # The gateway may be perfectly healthy — this is purely a missing
        # optional dependency, so surface the install hint directly rather
        # than the (misleading) gateway diagnosis.
        print(str(e))
        return 1
    except NoGatewayError:
        url = build_ws_url(host, port, ws_path)
        print(diagnose_no_gateway(url, config_path, workspace))
        return 1
    except AuthError as e:
        print(f"认证失败：{e}")
        return 1
    except KeyboardInterrupt:
        return 0
