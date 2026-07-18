from __future__ import annotations

import inspect

from echo_agent.gateway import server as server_mod


def test_gateway_http_event_reads_is_group_from_body():
    # HTTP 事件构造须从请求体取 is_group(默认 False),支持中继群消息标记。
    src = inspect.getsource(server_mod.GatewayServer)
    assert "is_group" in src
    # 事件构造处应把 body 的 is_group 传入 InboundEvent
    assert 'body.get("is_group"' in src


def test_gateway_ws_event_reads_is_group_from_data():
    # WS 入站路径也须从 data 取 is_group 并传入事件构造。
    import inspect
    from echo_agent.gateway import server as server_mod
    src = inspect.getsource(server_mod.GatewayServer)
    assert 'data.get("is_group"' in src
