"""向导收尾接管启动。

用户配完（尤其走「单独配置某个 section」路径）执行 echo-agent cli 只能得到连接
失败 —— 向导从不涉及「让进程跑起来」这一步，也从不提示它存在。
"""
import os as os_mod
import time as time_mod
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
    """打桩探针、确认框与服务动作。记录所有服务调用与所有提问以便断言。"""
    calls: list[tuple] = []
    asked: list[str] = []
    answers: list[bool] = []
    state = {"runtime": None}

    def _confirm(msg, default=True):
        asked.append(msg)
        return answers.pop(0) if answers else False

    monkeypatch.setattr(wiz, "probe_gateway", lambda **kw: state["runtime"])
    monkeypatch.setattr(wiz, "is_interactive", lambda: True)
    monkeypatch.setattr(wiz.ui, "confirm", _confirm)
    monkeypatch.setattr(
        wiz, "run_service_action",
        lambda action, **kw: calls.append((action, kw)) or 0,
    )
    # 屏蔽 Dashboard 构建询问。_offer_gateway_start 会先问「现在构建 Dashboard 吗」,
    # 这些用例预置的 answers 本意是回答启动询问,却会被构建询问先取走 —— 在装有
    # Node/pnpm 的机器(CI)上这会真的跑一遍 pnpm install + vite build,让一个讲服务
    # 启动的单元测试耗时数分钟并依赖网络。构建本身由 tests/gateway/ 下的用例覆盖。
    monkeypatch.setattr(wiz, "_maybe_offer_dashboard_build", lambda: None)
    # 启动后的确认轮询默认成功，避免每个用例都等 15 秒。
    # 真实轮询本身由 TestWaitUntilListening 直接测，不经过这个桩。
    monkeypatch.setattr(wiz, "_wait_until_listening", lambda *a, **kw: True)

    return SimpleNamespace(
        calls=calls, asked=asked, answers=answers, state=state,
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
    assert harness.asked == []  # 确认框根本没被调用


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
    # 「不代启」的另一半：也不问一个用户答了也没法执行的问题。少了这条断言，
    # 在这个分支里插一次 ui.confirm 仍然全绿。
    assert harness.asked == []
    out = harness.out()
    assert "tmux" in out


def test_disabled_explains_channels_still_work(harness):
    harness.state["runtime"] = _runtime(GatewayState.DISABLED)
    harness.answers.append(True)  # 网关本就没启用，不该拿启动来烦用户
    wiz._offer_gateway_start({"gateway": {"enabled": False}}, None)
    assert harness.calls == []
    assert harness.asked == []
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


def test_section_only_without_a_unit_points_at_install_not_start(harness):
    """无 unit 时 `gateway start` 会以 1 退出并叫用户改用 install。

    两种状态共用一条 not_running 文案会把用户引向一条注定失败的命令，
    所以 NOT_INSTALLED 要单独指向 install。
    """
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None, section_only=True)
    assert harness.calls == []
    assert harness.asked == []
    out = harness.out()
    assert "gateway install" in out
    assert "gateway start" not in out


def test_section_only_with_a_stopped_unit_points_at_start(harness):
    harness.state["runtime"] = _runtime(
        GatewayState.SERVICE_INSTALLED_STOPPED, service_installed=True,
    )
    harness.answers.append(True)
    wiz._offer_gateway_start(_config(), None, section_only=True)
    assert harness.calls == []
    assert harness.asked == []
    assert "gateway start" in harness.out()


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


# ── 真实轮询 ──────────────────────────────────────────────────────────────────

class TestWaitUntilListening:
    """直接测 _wait_until_listening 本身，不经过 harness 的打桩。

    「启动后必须轮询确认端口真的在监听」是本任务的核心约束：run_service_action
    返回 0 只代表 fork 成功，agent 仍可能在 bootstrap 阶段因 API key 无效退出。
    harness 无条件把这个函数换成 lambda: True，所以其余用例一个都碰不到真实函数体
    —— 把实现改成 `return True` 也不会有测试变红。这里补上那条防线。

    时钟与 sleep 都是假的：真跑满 15 秒窗口会让测试无法接受地慢。
    """

    @staticmethod
    def _fake_clock(monkeypatch, states):
        """让 probe_gateway 按 ``states`` 顺序返回，并用假时钟推进时间。

        每次 time.sleep(n) 把假时钟推进 n 秒，所以超时是确定性的、与真实耗时无关。
        """
        now = {"t": 0.0}
        probes: list[int] = []
        seq = list(states)

        def _probe(**kw):
            probes.append(1)
            state = seq.pop(0) if seq else GatewayState.NOT_INSTALLED
            return _runtime(state, listening=state is GatewayState.RUNNING)

        # _wait_until_listening imports time inside the function body, so the
        # patch has to land on the time module itself, not on an attribute of
        # echo_agent.cli.setup (there is none).
        monkeypatch.setattr(wiz, "probe_gateway", _probe)
        monkeypatch.setattr(time_mod, "monotonic", lambda: now["t"])
        monkeypatch.setattr(time_mod, "sleep", lambda n: now.__setitem__("t", now["t"] + n))
        return probes

    def test_returns_true_once_the_port_comes_up(self, monkeypatch):
        # 前两次还没起来，第三次开始监听。
        probes = self._fake_clock(monkeypatch, [
            GatewayState.SERVICE_INSTALLED_STOPPED,
            GatewayState.SERVICE_INSTALLED_STOPPED,
            GatewayState.RUNNING,
        ])
        assert wiz._wait_until_listening({}, None, None, timeout=15.0) is True
        assert len(probes) == 3  # 一起来就立刻返回，不白等剩下的窗口

    def test_returns_false_when_the_port_never_comes_up(self, monkeypatch):
        probes = self._fake_clock(monkeypatch, [])  # 永远 NOT_INSTALLED
        assert wiz._wait_until_listening({}, None, None, timeout=0.1) is False
        # t=0 探一次，此时还没到 deadline(0.1)，于是 sleep 到 t=0.5 再探一次，
        # 这次判定超时。窗口比轮询间隔短也至少探两次，不会一次都不探就放弃。
        assert len(probes) == 2

    def test_zero_timeout_still_probes_once(self, monkeypatch):
        """偏离 7 修正的行为：简报的 while 条件在 timeout=0 时一次都不探。

        启动刚返回时端口往往已经在听了，一次都不探就报失败是错的。
        """
        probes = self._fake_clock(monkeypatch, [GatewayState.RUNNING])
        assert wiz._wait_until_listening({}, None, None, timeout=0) is True
        assert len(probes) == 1

    def test_full_window_is_polled_repeatedly(self, monkeypatch):
        """15 秒窗口 / 0.5 秒间隔：冷启动要加载 embedding 模型，得多探几次。"""
        probes = self._fake_clock(monkeypatch, [])
        assert wiz._wait_until_listening({}, None, None, timeout=15.0) is False
        assert len(probes) == 31  # 第 1 次在 t=0，之后每 0.5 秒一次直到 t=15

    def test_a_raising_probe_propagates_rather_than_looping(self, monkeypatch):
        """探针契约是绝不抛异常。万一有人破了它，这里不吞异常也不空转 ——
        测试记录当前行为，好让契约破损立刻可见而不是变成一个静默的死循环。"""
        monkeypatch.setattr(time_mod, "monotonic", lambda: 0.0)
        monkeypatch.setattr(time_mod, "sleep", lambda n: None)

        def _boom(**kw):
            raise RuntimeError("probe contract broken")

        monkeypatch.setattr(wiz, "probe_gateway", _boom)
        with pytest.raises(RuntimeError):
            wiz._wait_until_listening({}, None, None, timeout=0)


# ── linger 提示 ───────────────────────────────────────────────────────────────

class TestLingerHint:
    """Linux 用户级服务退出登录就停，这对一个要 7x24 跑的东西是意外行为。"""

    @staticmethod
    def _linux(monkeypatch, loginctl_stdout):
        import subprocess
        import sys

        monkeypatch.setattr(sys, "platform", "linux")
        monkeypatch.setattr(os_mod, "geteuid", lambda: 1000, raising=False)
        monkeypatch.setattr(os_mod, "getlogin", lambda: "bob", raising=False)
        monkeypatch.setattr(
            subprocess, "run",
            lambda *a, **kw: SimpleNamespace(stdout=loginctl_stdout, returncode=0),
        )

    def test_warns_when_lingering_is_off(self, monkeypatch, capsys):
        self._linux(monkeypatch, "Linger=no\n")
        wiz._print_linger_hint_if_needed()
        out = capsys.readouterr().out
        assert "enable-linger" in out
        assert "bob" in out

    def test_silent_when_lingering_is_on(self, monkeypatch, capsys):
        self._linux(monkeypatch, "Linger=yes\n")
        wiz._print_linger_hint_if_needed()
        assert capsys.readouterr().out == ""

    def test_silent_on_non_linux(self, monkeypatch, capsys):
        import sys

        monkeypatch.setattr(sys, "platform", "darwin")
        wiz._print_linger_hint_if_needed()
        assert capsys.readouterr().out == ""

    def test_silent_when_loginctl_is_missing(self, monkeypatch, capsys):
        """容器里常没有 loginctl —— 探不到就闭嘴，不是报错。"""
        import subprocess
        import sys

        monkeypatch.setattr(sys, "platform", "linux")
        monkeypatch.setattr(os_mod, "geteuid", lambda: 1000, raising=False)
        monkeypatch.setattr(os_mod, "getlogin", lambda: "bob", raising=False)

        def _missing(*a, **kw):
            raise FileNotFoundError("loginctl")

        monkeypatch.setattr(subprocess, "run", _missing)
        wiz._print_linger_hint_if_needed()
        assert capsys.readouterr().out == ""


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


# ── 注册前的可启动性校验 ──────────────────────────────────────────────────────
#
# 旧默认（host=0.0.0.0 + 空 apiTokens）会被 _check_bind_safety 拒绝启动，而
# quickstart 从不进入 gateway 段、因此永远不会配 token —— 于是它注册出的服务
# 每次启动都失败。默认值已改回 127.0.0.1；这里守住"用户手动配成暴露态又没有
# token"时，向导必须当场解释而不是注册一个注定起不来的 unit。


def _exposed_config(**auth):
    cfg = {"gateway": {"enabled": True, "host": "0.0.0.0", "port": 58123}}
    if auth:
        cfg["gateway"]["auth"] = auth
    return cfg


def test_exposed_without_token_refuses_to_register(harness):
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)  # 若真去问了，这个 True 会让它注册

    wiz._offer_gateway_start(_exposed_config(), None)

    assert harness.calls == [], "不该注册或启动任何服务"
    assert harness.asked == [], "不该询问 —— 应直接解释原因"
    out = harness.out()
    assert "0.0.0.0" in out and ("127.0.0.1" in out or "apiTokens" in out)


def test_exposed_with_api_token_proceeds(harness):
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)

    wiz._offer_gateway_start(_exposed_config(api_tokens=["s3cret"]), None)

    assert [a for a, _ in harness.calls] == ["install", "start"]


def test_exposed_with_admin_token_only_proceeds(harness):
    """admin token 也算已认证 —— 与 _check_bind_safety 的口径一致，
    否则只配了 adminTokens 的部署会被误判为无法启动。"""
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)

    wiz._offer_gateway_start(_exposed_config(admin_tokens=["adm"]), None)

    assert [a for a, _ in harness.calls] == ["install", "start"]


def test_camel_case_token_keys_are_honoured(harness):
    """配置支持驼峰别名；只认下划线会把已配 token 的部署判成不可启动。"""
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)

    wiz._offer_gateway_start(_exposed_config(apiTokens=["s3cret"]), None)

    assert [a for a, _ in harness.calls] == ["install", "start"]


def test_loopback_without_token_is_fine(harness):
    """本机回环无需 token —— 这正是新的默认形态，必须能一路注册成功。"""
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)

    wiz._offer_gateway_start(_config(), None)

    assert [a for a, _ in harness.calls] == ["install", "start"]


def test_precheck_mirrors_server_bind_safety():
    """向导的判据是 server._check_bind_safety 的副本（向导只有一份可能尚未通过
    schema 校验的 YAML dict，构造 GatewayServer 去问代价过大）。这里逐组合比对
    两者结论，使任何一侧单独改动都会被发现。"""
    from pathlib import Path
    from unittest.mock import MagicMock

    from echo_agent.config.schema import Config
    from echo_agent.gateway.server import GatewayServer

    cases = [
        ("127.0.0.1", [], []),
        ("localhost", [], []),
        ("", [], []),
        ("0.0.0.0", [], []),
        ("0.0.0.0", ["t"], []),
        ("0.0.0.0", [], ["a"]),
        ("192.168.1.5", [], []),
        ("192.168.1.5", ["t"], []),
    ]
    for host, api, admin in cases:
        cfg = Config()
        cfg.gateway.host = host
        cfg.gateway.auth.api_tokens = list(api)
        cfg.gateway.auth.admin_tokens = list(admin)
        server = GatewayServer(
            cfg.gateway, MagicMock(), MagicMock(), MagicMock(), Path("."),
        )
        try:
            server._check_bind_safety()
            server_refuses = False
        except RuntimeError:
            server_refuses = True

        wizard_cfg = {"gateway": {
            "enabled": True, "host": host,
            "auth": {"api_tokens": list(api), "admin_tokens": list(admin)},
        }}
        wizard_refuses = bool(wiz._unstartable_reason(wizard_cfg))

        assert wizard_refuses == server_refuses, (
            f"host={host!r} api={api} admin={admin}: "
            f"向导判定 {wizard_refuses}，服务端判定 {server_refuses}"
        )


def test_disabled_gateway_is_not_flagged_unstartable():
    """gateway.enabled=false 时不存在"启动失败"问题，不该报不可启动。"""
    assert wiz._unstartable_reason(
        {"gateway": {"enabled": False, "host": "0.0.0.0"}}
    ) == ""
