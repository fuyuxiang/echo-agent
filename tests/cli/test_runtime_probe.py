"""网关运行时状态探针。

这是「网关是否真的可用」的唯一事实来源，被 status / doctor / cli 诊断共用。
它跑在用户已经不顺的失败路径上，所以任何子步骤失败都必须退化为「未知」而非抛异常。
"""
from types import SimpleNamespace

import pytest

from echo_agent.cli import runtime_probe
from echo_agent.cli.runtime_probe import GatewayState, probe_gateway


def _config(enabled=True, host="127.0.0.1", port=58123):
    return SimpleNamespace(
        gateway=SimpleNamespace(enabled=enabled, host=host, port=port),
        workspace=".",
    )


@pytest.fixture
def stub(monkeypatch, tmp_path):
    """打桩全部外部探测：TCP、endpoint 文件、服务后端。默认「什么都没有」。"""
    state = {"listening": False, "endpoint": None, "backend": None}

    monkeypatch.setattr(runtime_probe, "tcp_listening", lambda h, p, timeout=0.5: state["listening"])
    monkeypatch.setattr(runtime_probe, "_read_endpoint", lambda ws: state["endpoint"])
    monkeypatch.setattr(runtime_probe, "_resolve_workspace", lambda c, cp, ws: tmp_path)
    monkeypatch.setattr(runtime_probe, "_detect_backend", lambda: state["backend"])
    return state


def _backend(installed, running, name="systemd-user"):
    return SimpleNamespace(
        name=name,
        is_installed=lambda: installed,
        is_running=lambda: running,
    )


def test_disabled_when_gateway_off(stub):
    rt = probe_gateway(config=_config(enabled=False))
    assert rt.state is GatewayState.DISABLED


def test_running_when_port_is_listening(stub):
    stub["listening"] = True
    stub["backend"] = _backend(installed=True, running=True)
    rt = probe_gateway(config=_config())
    assert rt.state is GatewayState.RUNNING
    assert rt.listening is True


def test_running_wins_even_without_a_service(stub):
    # 前台 echo-agent gateway：端口在听但没有注册服务。仍然是可用的。
    stub["listening"] = True
    stub["backend"] = _backend(installed=False, running=False)
    assert probe_gateway(config=_config()).state is GatewayState.RUNNING


def test_service_installed_but_stopped(stub):
    stub["backend"] = _backend(installed=True, running=False)
    rt = probe_gateway(config=_config())
    assert rt.state is GatewayState.SERVICE_INSTALLED_STOPPED
    assert rt.service_installed is True


def test_service_running_but_port_dead_is_not_running(stub):
    # systemd 说 active，但端口不听：bootstrap 挂了。不能报 RUNNING。
    stub["backend"] = _backend(installed=True, running=True)
    rt = probe_gateway(config=_config())
    assert rt.state is GatewayState.SERVICE_INSTALLED_STOPPED
    assert rt.service_running is True
    assert rt.listening is False


def test_not_installed_when_manager_exists_but_no_unit(stub):
    stub["backend"] = _backend(installed=False, running=False)
    assert probe_gateway(config=_config()).state is GatewayState.NOT_INSTALLED


def test_no_service_manager_when_backend_is_none(stub):
    stub["backend"] = None
    rt = probe_gateway(config=_config())
    assert rt.state is GatewayState.NO_SERVICE_MANAGER
    assert rt.service_manager is None


def test_wildcard_host_is_probed_on_loopback(stub, monkeypatch):
    # 0.0.0.0 是 bind-only 通配符，探测必须打 127.0.0.1。
    seen = {}
    monkeypatch.setattr(
        runtime_probe, "tcp_listening",
        lambda h, p, timeout=0.5: seen.update(host=h, port=p) or True,
    )
    rt = probe_gateway(config=_config(host="0.0.0.0"))
    assert rt.probe_host == "127.0.0.1"
    assert seen["port"] == 58123


def test_dynamic_port_uses_the_endpoint_file(stub):
    # gateway.port=0 时真实端口只在 endpoint 文件里。
    stub["endpoint"] = {"port": 49152, "pid": 4242, "host": "127.0.0.1"}
    stub["listening"] = True
    rt = probe_gateway(config=_config(port=0))
    assert rt.bound_port == 49152
    assert rt.effective_port == 49152
    assert rt.pid == 4242


def test_dynamic_port_without_endpoint_is_not_running(stub):
    # port=0 且无 endpoint 文件：不能去连 :0，直接判定未运行。
    stub["endpoint"] = None
    rt = probe_gateway(config=_config(port=0))
    assert rt.listening is False
    assert rt.state is not GatewayState.RUNNING


def test_corrupt_endpoint_degrades_instead_of_raising(stub):
    stub["endpoint"] = {"port": "not-a-port"}
    rt = probe_gateway(config=_config())  # 不抛
    assert rt.state is not GatewayState.RUNNING


def test_unreadable_config_does_not_raise(monkeypatch):
    monkeypatch.setattr(
        runtime_probe, "_load_config",
        lambda cp, ws: (_ for _ in ()).throw(RuntimeError("boom")),
    )
    assert probe_gateway().state is GatewayState.DISABLED


def test_backend_that_raises_degrades_to_no_manager(stub, monkeypatch):
    class Exploding:
        name = "systemd-user"

        def is_installed(self):
            raise OSError("no bus")

        def is_running(self):
            return False

    monkeypatch.setattr(runtime_probe, "_detect_backend", lambda: Exploding())
    rt = probe_gateway(config=_config())  # 不抛
    assert rt.state in (GatewayState.NO_SERVICE_MANAGER, GatewayState.NOT_INSTALLED)


# ── 退化必须只丢失该丢的信息，不能答错 ────────────────────────────────────────
# 「不抛异常」不等于「答对」。下面两条锁住的是：某个字段读不出时，其余字段——
# 尤其调用方真正据以行动的 enabled——仍须忠实反映用户配置。

def test_infinite_endpoint_port_keeps_enabled_true(stub):
    # json.loads 接受非标准的 Infinity 字面量，而 int(inf) 抛 OverflowError。
    # 这个异常曾穿透到外层兜底，返回 enabled=False——用户明明开着网关，status
    # 却报「一切正常」(exit 0)，Task 4 还会让他去把已经是 true 的开关设为 true。
    stub["endpoint"] = {"host": "127.0.0.1", "port": float("inf"), "pid": 4242}
    rt = probe_gateway(config=_config(enabled=True))
    assert rt.enabled is True
    assert rt.state is not GatewayState.DISABLED
    assert rt.bound_port is None  # 端口不可用就是未知，而非瞎猜一个
    assert rt.listening is False


def test_infinite_configured_port_keeps_enabled_true(stub):
    # 同一个坑的 YAML 侧入口：port: .inf。
    rt = probe_gateway(config=_config(enabled=True, port=float("inf")))
    assert rt.enabled is True
    assert rt.state is not GatewayState.DISABLED
    assert rt.port == 0  # 读不出时用既有的「无可用端口」值 0，不是 None


# ── tcp_listening 本体（上面的用例都把它打桩掉了，这里不打桩） ────────────────

def test_tcp_listening_maps_wildcard_to_loopback():
    # 变异测试发现：删掉 tcp_listening 内部的通配符映射，全套测试仍绿——因为
    # 通配符用例打桩了 tcp_listening 本体，只验到 probe_host 属性。这条直接
    # 对着真实 socket 验：绑在 127.0.0.1 上的端口，用 0.0.0.0 去探必须探到。
    import socket as _socket

    srv = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    srv.bind(("127.0.0.1", 0))
    # Backlog must exceed the number of probes below: every successful probe
    # leaves a connection queued, and a full backlog makes later probes fail
    # for a reason that has nothing to do with the mapping under test.
    srv.listen(8)
    port = srv.getsockname()[1]
    try:
        for wildcard in ("0.0.0.0", "::", ""):
            assert runtime_probe.tcp_listening(wildcard, port) is True, wildcard
            conn, _ = srv.accept()  # drain, so the next probe sees a free slot
            conn.close()
    finally:
        srv.close()


def test_tcp_listening_never_dials_port_zero(monkeypatch):
    # port=0 且无 endpoint 文件时不能去连 :0。这条守卫若失守，connect 会真的
    # 发出去（:0 在部分平台上有诡异语义），所以断言压根没碰 socket。
    called = []
    monkeypatch.setattr(
        runtime_probe.socket, "create_connection",
        lambda *a, **k: called.append(a) or (_ for _ in ()).throw(AssertionError("dialed")),
    )
    assert runtime_probe.tcp_listening("127.0.0.1", 0) is False
    assert called == []


# ── 状态机的两条核心规则 ──────────────────────────────────────────────────────

def test_running_outranks_every_service_verdict(stub):
    # RUNNING 必须最高优先级：端口在听就是可用，无论服务后端怎么说。变异测试
    # 发现把优先级改成「先判 backend」时全套仍绿，故逐种 backend 组合钉死。
    stub["listening"] = True
    for installed, running in ((True, True), (True, False), (False, False)):
        stub["backend"] = _backend(installed=installed, running=running)
        assert probe_gateway(config=_config()).state is GatewayState.RUNNING
    stub["backend"] = None
    assert probe_gateway(config=_config()).state is GatewayState.RUNNING


def test_probe_targets_bound_port_not_configured_port(stub, monkeypatch):
    # 动态端口的要害：探测必须打 endpoint 里的真实端口，不能打配置里的那个。
    seen = {}
    monkeypatch.setattr(
        runtime_probe, "tcp_listening",
        lambda h, p, timeout=0.5: seen.update(port=p) or True,
    )
    stub["endpoint"] = {"host": "127.0.0.1", "port": 49152, "pid": 7}
    probe_gateway(config=_config(port=58123))
    assert seen["port"] == 49152


# ── 「绝不抛异常」这层保护本身 ────────────────────────────────────────────────

def test_probe_swallows_failures_from_any_sub_probe(stub, monkeypatch):
    # 外层 try 是模块契约的结构性保证，但它自己也需要被测试锁住：变异测试发现
    # 删掉它之后全套仍绿。逐个子探针注入异常，probe_gateway 都必须给出答案。
    for target in ("_resolve_workspace", "_read_endpoint", "_detect_backend", "tcp_listening"):
        monkeypatch.setattr(
            runtime_probe, target,
            lambda *a, **k: (_ for _ in ()).throw(RuntimeError("boom")),
        )
        rt = probe_gateway(config=_config())  # 不抛
        assert isinstance(rt, runtime_probe.GatewayRuntime)
        assert rt.state in tuple(GatewayState)
        monkeypatch.undo()


def test_service_manager_name_is_reported(stub):
    # service_manager 恒为 None 的变异此前存活；status 的 --json 用它决定
    # service 字段是 None 还是一个 dict，Task 3/4 也要靠它区分平台。
    stub["backend"] = _backend(installed=True, running=False, name="launchd")
    assert probe_gateway(config=_config()).service_manager == "launchd"
