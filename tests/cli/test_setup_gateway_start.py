"""向导收尾接管启动。

用户配完（尤其走「单独配置某个 section」路径）执行 echo-agent cli 只能得到连接
失败 —— 向导从不涉及「让进程跑起来」这一步，也从不提示它存在。
"""
from types import SimpleNamespace

import pytest

from echo_agent.cli import setup as wiz
from echo_agent.cli.i18n import get_locale, set_locale
from echo_agent.cli.runtime_probe import GatewayRuntime, GatewayState


@pytest.fixture(autouse=True)
def _restore_locale():
    """``run_setup_wizard`` re-detects and sets the process-global locale.

    Without this guard a wizard test run on a zh machine leaves the locale on zh
    and breaks locale-sensitive tests elsewhere, order-dependently.
    """
    saved = get_locale()
    try:
        yield
    finally:
        set_locale(saved)


@pytest.fixture
def harness(monkeypatch, capsys):
    """打桩探针、确认框与服务动作。记录所有服务调用以便断言。"""
    calls: list[tuple] = []
    answers: list[bool] = []
    state = {"runtime": None}

    monkeypatch.setattr(wiz, "probe_gateway", lambda **kw: state["runtime"])
    monkeypatch.setattr(wiz, "is_interactive", lambda: True)
    monkeypatch.setattr(
        wiz.ui, "confirm",
        lambda msg, default=True: answers.pop(0) if answers else False,
    )
    monkeypatch.setattr(
        wiz, "run_service_action",
        lambda action, **kw: calls.append((action, kw)) or 0,
    )
    # 启动后的确认轮询默认成功，避免每个用例都等 15 秒。
    monkeypatch.setattr(wiz, "_wait_until_listening", lambda *a, **kw: True)

    return SimpleNamespace(
        calls=calls, answers=answers, state=state,
        out=lambda: capsys.readouterr().out,
    )


def _runtime(state, **kw):
    return GatewayRuntime(state=state, enabled=state is not GatewayState.DISABLED,
                          host="127.0.0.1", port=58123, **kw)


def _config():
    return {"gateway": {"enabled": True, "host": "127.0.0.1", "port": 58123}}


def test_running_does_not_ask_or_touch_the_service(harness):
    harness.state["runtime"] = _runtime(GatewayState.RUNNING, listening=True)
    wiz._offer_gateway_start(_config(), None)
    assert harness.calls == []
    assert "echo-agent cli" in harness.out()


def test_installed_but_stopped_starts_after_consent(harness):
    harness.state["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED, service_installed=True,
    )
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None)
    assert [a for a, _ in harness.calls] == ["start"]


def test_active_unit_with_a_dead_port_is_restarted_not_started(harness):
    """探针把「systemd 说 active 但端口是死的」也归到 SERVICE_INSTALLED_STOPPED。

    对一个已 active 的 unit 执行 start 是空操作，会白等满一个轮询窗口后仍然失败；
    这种情况要 restart。
    """
    harness.state["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED,
        service_installed=True, service_running=True,
    )
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None)
    assert [a for a, _ in harness.calls] == ["restart"]


def test_declining_start_touches_nothing(harness):
    harness.state["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED, service_installed=True,
    )
    harness.answers.append(False)
    wiz._offer_gateway_start(_config(), None)
    assert harness.calls == []


def test_not_installed_installs_then_starts(harness):
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None)
    assert [a for a, _ in harness.calls] == ["install", "start"]


def test_root_under_installer_defers_registration(harness, monkeypatch):
    """install.sh 以 root 运行时注册的是 system unit，向导只会注册 user unit。

    两边都注册就会有两个 unit 抢同一个端口和工作区锁，所以带着标记且身为 root 时
    向导让位给 installer。
    """
    monkeypatch.setattr(wiz, "_installer_owns_service_registration", lambda: True)
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)  # 即使用户会同意，也不该被问
    wiz._offer_gateway_start(_config(), None)
    assert harness.calls == []
    assert harness.answers == [True]  # 确认框根本没被调用


def test_installer_flag_alone_does_not_defer(monkeypatch):
    """普通用户下 installer 与向导的 scope 相同，向导照常注册。"""
    import os
    import sys

    monkeypatch.setenv("ECHO_AGENT_SETUP_HANDLES_SERVICE", "1")
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(os, "geteuid", lambda: 1000)
    assert wiz._installer_owns_service_registration() is False


def test_no_installer_flag_never_defers(monkeypatch):
    """没有 installer 标记时，即便是 root 也照常询问 —— 这是手工 sudo setup 的场景。"""
    import os
    import sys

    monkeypatch.delenv("ECHO_AGENT_SETUP_HANDLES_SERVICE", raising=False)
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(os, "geteuid", lambda: 0)
    assert wiz._installer_owns_service_registration() is False


def test_root_with_installer_flag_defers(monkeypatch):
    import os
    import sys

    monkeypatch.setenv("ECHO_AGENT_SETUP_HANDLES_SERVICE", "1")
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(os, "geteuid", lambda: 0)
    assert wiz._installer_owns_service_registration() is True


def test_no_service_manager_never_calls_a_service_action(harness):
    """「只给命令，不代启」的回归防线。

    向导不应遗留一个它管不了的后台进程：无监管、崩溃不拉起、重启不自启，
    且容易与单实例锁冲突。
    """
    harness.state["runtime"] = _runtime(GatewayState.NO_SERVICE_MANAGER)
    harness.answers.extend([True, True, True])  # 即使用户什么都同意
    wiz._offer_gateway_start(_config(), None)
    assert harness.calls == []
    out = harness.out()
    assert "tmux" in out


def test_disabled_explains_channels_still_work(harness):
    harness.state["runtime"] = _runtime(GatewayState.DISABLED)
    wiz._offer_gateway_start({"gateway": {"enabled": False}}, None)
    assert harness.calls == []
    assert "echo-agent setup gateway" in harness.out()


def test_non_interactive_is_a_noop(harness, monkeypatch):
    monkeypatch.setattr(wiz, "is_interactive", lambda: False)
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    wiz._offer_gateway_start(_config(), None)
    assert harness.calls == []


def test_start_that_never_listens_points_at_the_logs(harness, monkeypatch):
    # systemd 的 start 返回 0 只代表 fork 成功；bootstrap 可能因 API key 错误退出。
    monkeypatch.setattr(wiz, "_wait_until_listening", lambda *a, **kw: False)
    harness.state["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED, service_installed=True,
    )
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None)
    assert "gateway logs" in harness.out()


def test_section_only_never_asks_but_still_warns(harness):
    """单独改配置时不该被问「要不要装服务」，但必须提示重启才生效。"""
    harness.state["runtime"] = _runtime(GatewayState.RUNNING, listening=True)
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None, section_only=True)
    assert harness.calls == []
    assert "restart" in harness.out()


def test_a_failing_service_action_does_not_abort_the_wizard(harness, monkeypatch):
    """`systemctl start` 失败时后端会 sys.exit —— 那会连摘要都打不出来。

    service.base.run(check=True) 在非零返回码时直接退出进程，backend.start()
    在未安装时也 raise SystemExit。配置已经落盘，启动失败只是一个要汇报的结果，
    不该把整个 setup 拖成非零退出。
    """
    def _boom(action, **kw):
        harness.calls.append((action, kw))
        raise SystemExit(1)

    monkeypatch.setattr(wiz, "run_service_action", _boom)
    harness.state["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED, service_installed=True,
    )
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None)  # 不抛异常
    assert [a for a, _ in harness.calls] == ["start"]
    assert "gateway logs" in harness.out()


def test_unreadable_port_does_not_crash_the_handoff(harness):
    """手改过的 YAML 可能带 port: abc —— 收尾阶段不该因此抛栈。"""
    harness.state["runtime"] = _runtime(GatewayState.RUNNING, listening=True)
    wiz._offer_gateway_start({"gateway": {"enabled": True, "port": "abc"}}, None)
    assert harness.calls == []


# ── 接入点回归 ────────────────────────────────────────────────────────────────

def test_single_section_path_reaches_the_handoff(monkeypatch, tmp_path):
    """本次 bug 的入口：走「单独配置：消息渠道」保存后直接 return，
    既不打印摘要也不提示需要启动。"""
    seen: list[dict] = []
    monkeypatch.setattr(
        wiz, "_offer_gateway_start",
        lambda cfg, path, ws=None, **kw: seen.append(kw),
    )
    monkeypatch.setattr(wiz, "is_interactive", lambda: True)
    monkeypatch.setattr(wiz, "setup_channels", lambda config: None)
    monkeypatch.setattr(wiz, "_print_banner", lambda: None)
    monkeypatch.setattr(
        wiz, "SETUP_SECTIONS", [("channel", lambda config: None)],
    )
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("models:\n  providers:\n    - name: openai\n", encoding="utf-8")

    rc = wiz.run_setup_wizard(section="channel", config_path=str(cfg))

    assert rc == 0
    assert seen and seen[0].get("section_only") is True


def test_menu_section_path_reaches_the_handoff(monkeypatch, tmp_path):
    """菜单里选「单独配置：消息渠道」—— 用户实际踩到的那条路径。"""
    seen: list[dict] = []
    monkeypatch.setattr(
        wiz, "_offer_gateway_start",
        lambda cfg, path, ws=None, **kw: seen.append(kw),
    )
    monkeypatch.setattr(wiz, "is_interactive", lambda: True)
    monkeypatch.setattr(wiz, "_print_banner", lambda: None)
    monkeypatch.setattr(wiz, "ui", _stub_ui(select_returns="section:channel"))
    monkeypatch.setattr(
        wiz, "SETUP_SECTIONS", [("channel", lambda config: None)],
    )
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("models:\n  providers:\n    - name: openai\n", encoding="utf-8")

    rc = wiz.run_setup_wizard(config_path=str(cfg))

    assert rc == 0
    assert seen and seen[0].get("section_only") is True


def test_full_flow_reaches_the_handoff_without_section_only(monkeypatch, tmp_path):
    """全量流程该拿到「装服务」的完整询问，而不是 section 的只读提示。"""
    seen: list[dict] = []
    monkeypatch.setattr(
        wiz, "_offer_gateway_start",
        lambda cfg, path, ws=None, **kw: seen.append(kw),
    )
    monkeypatch.setattr(wiz, "is_interactive", lambda: True)
    monkeypatch.setattr(wiz, "_print_banner", lambda: None)
    monkeypatch.setattr(wiz, "setup_doctor", lambda config: None)
    monkeypatch.setattr(wiz, "_ensure_credential_key", lambda ws: None)
    monkeypatch.setattr(
        wiz, "SETUP_SECTIONS", [("gateway", lambda config: None)],
    )
    cfg = tmp_path / "echo-agent.yaml"

    rc = wiz.run_setup_wizard(config_path=str(cfg), flow="full")

    assert rc == 0
    assert seen and seen[0].get("section_only") in (None, False)


def test_quickstart_reaches_the_handoff(monkeypatch, tmp_path):
    seen: list[dict] = []
    monkeypatch.setattr(
        wiz, "_offer_gateway_start",
        lambda cfg, path, ws=None, **kw: seen.append(kw),
    )
    monkeypatch.setattr(wiz, "is_interactive", lambda: True)
    monkeypatch.setattr(wiz, "_print_banner", lambda: None)
    monkeypatch.setattr(wiz, "setup_doctor", lambda config: None)
    monkeypatch.setattr(wiz, "_ensure_credential_key", lambda ws: None)
    for name in ("setup_language", "setup_model", "setup_permissions"):
        monkeypatch.setattr(wiz, name, lambda config: None)
    cfg = tmp_path / "echo-agent.yaml"

    rc = wiz.run_setup_wizard(config_path=str(cfg), flow="quickstart")

    assert rc == 0
    assert seen and seen[0].get("section_only") in (None, False)


def _stub_ui(select_returns: str):
    """A ui module stand-in: only ``select`` is scripted, output is inert."""
    return SimpleNamespace(
        select=lambda *a, **kw: select_returns,
        confirm=lambda *a, **kw: False,
        intro=lambda *a, **kw: None,
        outro=lambda *a, **kw: None,
        note=lambda *a, **kw: None,
        Choice=tuple,
    )
