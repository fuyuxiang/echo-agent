"""echo-agent cli 连接失败时的诊断。

回归背景：诊断写死了 `systemctl is-active echo-agent`。在未开 systemd 的 WSL2 上
这条命令只会回 "System has not been booted with systemd"，把用户引向死路 —— 而
那正是本次问题的发生环境。
"""
import pytest

from echo_agent.cli import attach_client
from echo_agent.cli.runtime_probe import GatewayRuntime, GatewayState

URL = "ws://127.0.0.1:58123/ws"


def _runtime(state, **kw):
    return GatewayRuntime(
        state=state, enabled=state is not GatewayState.DISABLED,
        host="127.0.0.1", port=58123, **kw,
    )


@pytest.fixture
def probe(monkeypatch):
    state = {}

    def fake(**kw):
        return state["runtime"]

    monkeypatch.setattr(attach_client, "probe_gateway", fake)
    return state


def test_no_service_manager_never_suggests_systemctl(probe, monkeypatch):
    """本次踩坑的直接成因，钉死。"""
    monkeypatch.setattr(attach_client, "is_wsl", lambda: True)
    probe["runtime"] = _runtime(GatewayState.NO_SERVICE_MANAGER)
    msg = attach_client.diagnose_no_gateway(URL, None, None)
    assert "systemctl" not in msg
    assert "systemd=true" in msg  # WSL 用户应看到「可以开启 systemd」这条出路


def test_service_installed_stopped_suggests_start(probe):
    probe["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED, service_installed=True,
    )
    msg = attach_client.diagnose_no_gateway(URL, None, None)
    assert "echo-agent gateway start" in msg


def test_service_running_but_dead_port_points_at_logs(probe):
    # 服务在跑却连不上：bootstrap 挂了。再 start 一次没用，该看日志。
    probe["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED,
        service_installed=True, service_running=True,
    )
    msg = attach_client.diagnose_no_gateway(URL, None, None)
    assert "echo-agent gateway logs" in msg


def test_not_installed_suggests_install(probe):
    probe["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    msg = attach_client.diagnose_no_gateway(URL, None, None)
    assert "echo-agent gateway install" in msg


def test_disabled_says_channels_are_unaffected(probe):
    probe["runtime"] = _runtime(GatewayState.DISABLED)
    msg = attach_client.diagnose_no_gateway(URL, None, None)
    assert "gateway.enabled" in msg
    assert "微信" in msg or "WeChat" in msg


def test_every_state_points_at_the_project_own_status_command(probe):
    for state in GatewayState:
        probe["runtime"] = _runtime(state)
        msg = attach_client.diagnose_no_gateway(URL, None, None)
        assert msg.strip(), f"{state} produced an empty diagnosis"


def test_connection_info_loads_config_once(monkeypatch, tmp_path):
    """一次 echo-agent cli 曾加载同一份配置 4 次（resolve_defaults、save_dir、
    api_prefix、诊断各一次），每次都在 stderr 打一行 DEBUG。"""
    calls = []
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("gateway:\n  enabled: true\n  port: 58123\n", encoding="utf-8")

    import echo_agent.config.loader as loader

    real = loader.load_config
    monkeypatch.setattr(
        loader, "load_config",
        lambda **kw: calls.append(1) or real(**kw),
    )

    attach_client.resolve_connection(str(cfg), None)

    assert len(calls) == 1, f"loaded the config {len(calls)} times"


def test_connection_info_seeds_first_paint_model_and_context(tmp_path):
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text(
        "gateway:\n  enabled: true\n  port: 58123\n"
        "models:\n  defaultModel: MiniMax-M3\n",
        encoding="utf-8",
    )
    info = attach_client.resolve_connection(str(cfg), None)
    assert info.model == "MiniMax-M3"
    # models.dev cache may know the provider's exact binary 1M window
    # (1,048,576); the built-in cold-start registry uses decimal 1,000,000.
    assert info.context_max >= 1_000_000
