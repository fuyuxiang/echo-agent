import asyncio

import aiohttp
import pytest

from echo_agent.cli.attach_client import (
    AuthError,
    MissingTUIDependencyError,
    NoGatewayError,
    OutboundRenderer,
    authenticate,
    build_ws_url,
    connect_ws,
    diagnose_no_gateway,
)


def test_build_ws_url_loopback():
    assert build_ws_url("127.0.0.1", 9000, "/ws") == "ws://127.0.0.1:9000/ws"


def test_build_ws_url_normalizes_missing_leading_slash():
    assert build_ws_url("127.0.0.1", 9000, "ws") == "ws://127.0.0.1:9000/ws"


@pytest.mark.asyncio
async def test_connect_ws_raises_no_gateway_on_refused():
    import aiohttp
    async with aiohttp.ClientSession() as s:
        # 9 是 discard 端口，本机通常无监听 → 连接被拒
        with pytest.raises(NoGatewayError) as ei:
            await connect_ws(s, "ws://127.0.0.1:9/ws")
    # connect_ws now carries only the bare url; actionable guidance is built
    # separately by diagnose_no_gateway (which needs the config path).
    assert "ws://127.0.0.1:9/ws" in str(ei.value)


def test_diagnose_gateway_disabled(tmp_path):
    cfg = tmp_path / "off.yaml"
    cfg.write_text("gateway:\n  enabled: false\n")
    msg = diagnose_no_gateway("ws://127.0.0.1:58123/ws", str(cfg), None)
    assert "gateway.enabled=false" in msg
    assert "true" in msg  # points the user at the fix


def test_diagnose_gateway_enabled_unreachable(tmp_path):
    cfg = tmp_path / "on.yaml"
    cfg.write_text("gateway:\n  enabled: true\n  host: 127.0.0.1\n  port: 58123\n")
    msg = diagnose_no_gateway("ws://127.0.0.1:58123/ws", str(cfg), None)
    assert "gateway.enabled=true" in msg


def test_renderer_streams_then_finalizes_without_dup(capsys):
    r = OutboundRenderer()
    r.render({
        "type": "message", "text": "你好", "is_final": False,
        "metadata": {"_token_stream": True, "_inbound_event_id": "e1"},
    })
    r.render({
        "type": "message", "text": "，世界", "is_final": False,
        "metadata": {"_token_stream": True, "_inbound_event_id": "e1"},
    })
    r.render({
        "type": "message", "text": "你好，世界", "is_final": True,
        "message_kind": "final",
        "metadata": {"_token_stream": True, "_inbound_event_id": "e1"},
    })
    out = capsys.readouterr().out
    # 流式两段 + 最终只补余下，全文 "你好，世界" 只出现一次
    assert out.count("你好，世界") == 1


def test_renderer_non_stream_prints_text(capsys):
    r = OutboundRenderer()
    r.render({"type": "message", "text": "完整回复", "is_final": True, "metadata": {}})
    assert "完整回复" in capsys.readouterr().out


class _FakeWS:
    """Minimal ws stub: records sent frames, replays a queued receive_json."""

    def __init__(self, reply: dict) -> None:
        self.sent: list[dict] = []
        self._reply = reply

    async def send_json(self, data: dict) -> None:
        self.sent.append(data)

    async def receive_json(self) -> dict:
        return self._reply


@pytest.mark.asyncio
async def test_authenticate_returns_session_key_on_ok():
    ws = _FakeWS({"type": "auth_ok", "session_key": "sk-server"})
    key = await authenticate(
        ws, platform="cli", user_id="u1", session_key="sk-client", token="t"
    )
    assert key == "sk-server"
    assert ws.sent[0]["type"] == "auth"
    assert ws.sent[0]["platform"] == "cli"


@pytest.mark.asyncio
async def test_authenticate_raises_on_error():
    ws = _FakeWS({"type": "error", "error": "bad token"})
    with pytest.raises(AuthError) as ei:
        await authenticate(
            ws, platform="cli", user_id="u1", session_key="sk", token="t"
        )
    assert "bad token" in str(ei.value)


@pytest.mark.asyncio
async def test_client_auth_and_send_roundtrip(gateway_ws_url):
    # 不走完整 run_client 的 stdin，只验证 connect+auth+发 message 被服务端 accepted
    async with aiohttp.ClientSession() as s:
        ws = await connect_ws(s, gateway_ws_url)
        sk = await authenticate(
            ws, platform="cli", user_id="alice",
            session_key="cli:alice", token="",
        )
        assert sk == "cli:alice"
        await ws.send_json({"type": "message", "text": "ping"})
        # 服务端先回 accepted（agent 回复异步，到不到取决于是否挂了真 agent）
        msg = await asyncio.wait_for(ws.receive_json(), timeout=5)
        assert msg["type"] in ("accepted", "message")
        await ws.close()


def test_require_textual_raises_missing_dep_when_absent(monkeypatch):
    import builtins
    from echo_agent.cli import attach_client

    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name == "textual" or name.startswith("textual."):
            raise ImportError("No module named 'textual'")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", fake_import)
    with pytest.raises(MissingTUIDependencyError) as ei:
        attach_client._require_textual()
    assert "textual" in str(ei.value)


def test_require_textual_rejects_below_floor(monkeypatch):
    """低于最低版本时须报明确的版本错误，而非放行到深处崩溃。

    TUI 依赖 textual 的 theme API (0.86) 与 content markup (2.0)，实际按
    8.2 验证；低于 floor 的旧版会 import 失败或渲染时 MarkupError。"""
    import textual
    from echo_agent.cli import attach_client

    for low in ("0.60.0", "0.85.1", "2.0.0", "8.1.9"):
        monkeypatch.setattr(textual, "__version__", low, raising=False)
        with pytest.raises(MissingTUIDependencyError) as ei:
            attach_client._require_textual()
        # 错误须点明已装版本与所需下限，给出可操作指引
        assert low in str(ei.value)
        assert "8.2" in str(ei.value)


def test_require_textual_accepts_floor_and_above(monkeypatch):
    """>= floor（含预发布后缀）须放行。"""
    import textual
    from echo_agent.cli import attach_client

    for ok in ("8.2.0", "8.2.8", "9.0.0rc1", "10.0.0"):
        monkeypatch.setattr(textual, "__version__", ok, raising=False)
        # 不抛异常即为通过
        attach_client._require_textual()


@pytest.mark.asyncio
async def test_run_client_passes_handshake_session_key_into_tui(monkeypatch):
    import echo_agent.cli.attach_client as ac

    captured = {}

    class _WS:
        def __init__(self):
            self.closed = False
        async def send_json(self, data):
            self._auth_reply = {"type": "auth_ok", "session_key": "cli:server-side"}
        async def receive_json(self):
            return self._auth_reply
        def __aiter__(self):
            return self
        async def __anext__(self):
            raise StopAsyncIteration  # pump 立即结束
        async def close(self):
            self.closed = True

    ws = _WS()

    async def fake_connect(session, url):
        return ws
    monkeypatch.setattr(ac, "connect_ws", fake_connect)

    class _FakeApp:
        def __init__(self, send_coro=None, session_key="", interrupt_coro=None,
                     reconnect_coro=None):
            captured["session_key"] = session_key
        def notify_disconnected(self):
            pass
        async def run_async(self):
            return None
    monkeypatch.setattr("echo_agent.cli.tui.app.EchoTUI", _FakeApp)

    class _FakeBridge:
        def __init__(self, sink):
            pass
    monkeypatch.setattr("echo_agent.cli.tui.bridge.WSBridge", _FakeBridge)
    monkeypatch.setattr(ac, "_require_textual", lambda: None)

    rc = await ac.run_client(
        host="127.0.0.1", port=58123, ws_path="/ws", user_id="local", token="",
    )
    assert rc == 0
    assert captured["session_key"] == "cli:server-side"


def test_run_cli_attach_missing_textual_prints_install_hint_not_gateway(
    monkeypatch, capsys
):
    """缺 textual 时须给出清晰安装提示，且绝不误报网关缺失诊断。"""
    from echo_agent.cli import attach_client

    def boom() -> None:
        raise MissingTUIDependencyError(
            "缺少 TUI 依赖 textual。请安装：pip install \"echo-agent[all]\" "
            "或 pip install \"echo-agent[tui]\"。"
        )

    monkeypatch.setattr(attach_client, "_require_textual", boom)
    rc = attach_client.run_cli_attach(
        host="127.0.0.1", port=58123, ws_path="/ws", user_id="u1", token="",
    )
    out = capsys.readouterr().out
    assert rc == 1
    assert "textual" in out
    # 必须 NOT 落到网关诊断分支
    assert "未发现本机常驻 echo-agent" not in out
    assert "gateway.enabled" not in out


def test_resolve_defaults_reads_static_port(tmp_path, monkeypatch):
    """普通固定端口：直接从配置读取。"""
    from echo_agent.cli import attach_client as ac
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("gateway:\n  enabled: true\n  port: 51999\n  wsPath: /ws\n")
    host, port, ws_path, _token = ac.resolve_defaults(str(cfg), None)
    assert host == "127.0.0.1"
    assert port == 51999
    assert ws_path == "/ws"


def test_resolve_defaults_port_zero_falls_back_to_runtime_endpoint(tmp_path):
    """gateway.port=0（系统动态分配）时，attach 必须从 gateway 写入的运行时
    端点文件读到真实端口，否则会连到 127.0.0.1:0 必然失败。"""
    from echo_agent.cli import attach_client as ac
    from echo_agent.cli.workspace import write_runtime_endpoint
    cfg = tmp_path / "echo-agent.yaml"
    # workspace 相对配置文件目录解析 → tmp_path
    cfg.write_text(
        "workspace: .\n"
        "gateway:\n  enabled: true\n  port: 0\n  wsPath: /ws\n"
    )
    # 模拟 gateway 绑定后写入真实端口。
    write_runtime_endpoint(tmp_path, host="127.0.0.1", port=62345,
                           pid=1234, ws_path="/ws")
    host, port, ws_path, _token = ac.resolve_defaults(str(cfg), None)
    assert host == "127.0.0.1"
    assert port == 62345          # 来自运行时端点，而非配置里的 0
    assert ws_path == "/ws"


def test_resolve_defaults_port_zero_without_endpoint_stays_zero(tmp_path):
    """端点文件不存在时不崩溃，退回配置端口（0）。上层据此报错，而非误连。"""
    from echo_agent.cli import attach_client as ac
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text(
        "workspace: .\n"
        "gateway:\n  enabled: true\n  port: 0\n  wsPath: /ws\n"
    )
    _host, port, _ws_path, _token = ac.resolve_defaults(str(cfg), None)
    assert port == 0
