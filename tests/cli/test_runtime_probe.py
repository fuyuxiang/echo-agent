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
