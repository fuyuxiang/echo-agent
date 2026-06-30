"""echo-agent cli — thin WebSocket client that attaches to a running
gateway on loopback and opens its own (cli:) session. Builds no agent."""

from __future__ import annotations

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
