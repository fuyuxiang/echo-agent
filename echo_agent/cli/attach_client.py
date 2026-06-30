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
