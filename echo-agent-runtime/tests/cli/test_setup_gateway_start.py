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
        import sys

        monkeypatch.setattr(sys, "platform", "linux")
        monkeypatch.setattr(os_mod, "geteuid", lambda: 1000, raising=False)
        monkeypatch.setattr(os_mod, "getlogin", lambda: "bob", raising=False)
        monkeypatch.setattr(
            wiz, "run_owned",
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
        import sys

        monkeypatch.setattr(sys, "platform", "linux")
        monkeypatch.setattr(os_mod, "geteuid", lambda: 1000, raising=False)
        monkeypatch.setattr(os_mod, "getlogin", lambda: "bob", raising=False)

        def _missing(*a, **kw):
            raise FileNotFoundError("loginctl")

        monkeypatch.setattr(wiz, "run_owned", _missing)
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
    两者结论，使任何一侧单独改动都会被发现。

    两者的 host 分类现在共用 gateway/host_rules.py。曾经各自持有一份字符串
    元组，而元组里都含 ""——那其实是通配绑定（aiohttp 把 "" 绑到 0.0.0.0 与
    ::），于是「host 为空 + 无 token」被双方一致判为可启动，把一个未认证的
    网关暴露到网络。下面的 ""/"::"/"[::1]"/"127.0.0.2" 就是当年分歧所在，
    必须留在用例里。"""
    from pathlib import Path
    from unittest.mock import MagicMock

    from echo_agent.config.schema import Config
    from echo_agent.gateway.server import GatewayServer

    cases = [
        ("127.0.0.1", [], []),
        ("localhost", [], []),
        # 通配绑定：无 token 时双方都必须拒绝
        ("", [], []),
        ("", ["t"], []),
        ("::", [], []),
        ("[::]", [], []),
        ("0.0.0.0", [], []),
        ("0.0.0.0", ["t"], []),
        ("0.0.0.0", [], ["a"]),
        # 127/8 全段与 IPv6 回环：都是本机，无 token 也应放行
        ("127.0.0.2", [], []),
        ("::1", [], []),
        ("[::1]", [], []),
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


# ── 非回环绑定 + 空 allowed_hosts：可启动但管理端点全被 403 ────────────────────
#
# is_host_allowed（gateway/auth.py）的 DNS-rebinding 护栏在「非回环绑定 + 无
# 可用 allowed_hosts」下拒绝一切 Host。爆炸半径限于管理端点：该检查在
# _check_csrf 里，而 _check_csrf 只被 _require_admin_token 调用，所以登录、
# 只读页与本机 cli/curl 仍可用，而会话 / 配置 / 记忆写入 / 任务 / 定时 /
# 知识库全部 403。向导过去从不追问 allowed_hosts，于是 0.0.0.0 + token 的部署
# 「保存成功」却在浏览器里用不了管理功能。这组用例守住探测函数的判据，与
# server._warn_host_allowlist_if_unset 同源。


def test_browser_unreachable_when_exposed_without_allowed_hosts():
    assert wiz._browser_unreachable_reason(_exposed_config(api_tokens=["t"])) == "0.0.0.0"


def test_browser_reachable_once_allowed_hosts_listed():
    cfg = _exposed_config(api_tokens=["t"], allowed_hosts=["echo.example.com"])
    assert wiz._browser_unreachable_reason(cfg) == ""


def test_exposed_dashboard_without_allowed_origin_warns_compatibility():
    cfg = _exposed_config(api_tokens=["t"], allowed_hosts=["echo.example.com"])
    assert wiz._browser_origin_compat_reason(cfg) == "0.0.0.0"


def test_exposed_dashboard_with_allowed_origin_has_no_compat_warning():
    cfg = _exposed_config(api_tokens=["t"], allowed_hosts=["echo.example.com"])
    cfg["gateway"]["auth"]["allowed_origins"] = ["https://echo.example.com"]
    assert wiz._browser_origin_compat_reason(cfg) == ""


def test_origin_compat_warning_honours_camelcase_and_normalization():
    cfg = _exposed_config(apiTokens=["t"], allowedHosts=["echo.example.com"])
    cfg["gateway"]["auth"]["allowedOrigins"] = ["https://Echo.Example.com:443/"]
    assert wiz._browser_origin_compat_reason(cfg) == ""


def test_missing_host_uses_stronger_warning_only():
    cfg = _exposed_config(api_tokens=["t"])
    assert wiz._browser_unreachable_reason(cfg) == "0.0.0.0"
    assert wiz._browser_origin_compat_reason(cfg) == ""


def test_browser_unreachable_honours_camelcase_allowed_hosts():
    """配置支持驼峰别名；只认下划线会把已配 allowedHosts 的部署误报为不可达。"""
    cfg = _exposed_config(apiTokens=["t"], allowedHosts=["echo.example.com"])
    assert wiz._browser_unreachable_reason(cfg) == ""


def test_loopback_bind_never_flagged_browser_unreachable():
    """回环绑定下空 allowed_hosts 会默认放行 localhost/127.0.0.1，不该报不可达。"""
    assert wiz._browser_unreachable_reason(_config()) == ""


def test_disabled_gateway_is_not_flagged_browser_unreachable():
    assert wiz._browser_unreachable_reason(
        {"gateway": {"enabled": False, "host": "0.0.0.0"}}
    ) == ""


def test_exposed_with_token_but_no_allowed_hosts_warns_yet_proceeds(harness):
    """能启动，仍要注册 + 启动，但必须打出浏览器不可达的警告。"""
    harness.state["runtime"] = _runtime(GatewayState.NOT_INSTALLED)
    harness.answers.append(True)

    wiz._offer_gateway_start(_exposed_config(api_tokens=["s3cret"]), None)

    assert [a for a, _ in harness.calls] == ["install", "start"]
    assert "allowed_hosts" in harness.out()


def test_wildcard_allowed_hosts_entry_does_not_suppress_the_warning():
    """allowed_hosts=[0.0.0.0] 不算已配置。

    浏览器发送的是地址栏里的名字，永远不会是 0.0.0.0，所以这种条目匹配不到任何
    请求，却因为「列表非空」抑制了告警——向导用绑定地址预填时正是这么写出来的。
    """
    cfg = _exposed_config(api_tokens=["t"], allowed_hosts=["0.0.0.0"])
    assert wiz._browser_unreachable_reason(cfg) == "0.0.0.0"
    cfg = _exposed_config(api_tokens=["t"], allowed_hosts=["::", ""])
    assert wiz._browser_unreachable_reason(cfg) == "0.0.0.0"


def test_empty_host_is_reported_not_silently_passed():
    """host: "" 是通配绑定，两个探测都必须报问题且给出可读的名字。

    返回值同时充当「有没有问题」的真值信号，直接回传空串会把暴露态报成正常——
    这正是修复前的行为。
    """
    unstartable = wiz._unstartable_reason({"gateway": {"enabled": True, "host": ""}})
    assert unstartable, "host 为空 + 无 token 必须判为不可启动"
    assert "0.0.0.0" in unstartable

    unreachable = wiz._browser_unreachable_reason(
        {"gateway": {"enabled": True, "host": "", "auth": {"api_tokens": ["t"]}}}
    )
    assert unreachable
    assert "0.0.0.0" in unreachable


def test_loopback_variants_are_not_flagged():
    """127/8 全段与 IPv6 回环都是本机，不该报不可达（旧字符串判据会误报）。"""
    for host in ("127.0.0.2", "::1", "[::1]", "localhost"):
        cfg = {"gateway": {"enabled": True, "host": host, "port": 58123}}
        assert wiz._browser_unreachable_reason(cfg) == "", host
        assert wiz._unstartable_reason(cfg) == "", host


# ── setup_gateway 真正保存下来的配置 ──────────────────────────────────────────
#
# 上面那组只验证探测函数与告警文案。真正的失败模式在于「向导一路回车之后落盘的
# 是什么」：探测函数可以全部正确，而向导仍然写出一份 allowed_hosts=[0.0.0.0] 的
# 无效配置——它既匹配不到任何浏览器请求，又让告警闭嘴。所以这组用例直接调
# setup_gateway，断言最终 config dict。


@pytest.fixture
def gateway_wizard(monkeypatch, capsys):
    """驱动 setup_gateway：按脚本回答 text/confirm，返回落盘的 gateway 配置。

    text 的答案用 None 表示「直接回车」——ui.text 在空输入时回落到 default，
    而「用户是否拒绝了这一步」只能在过滤之后才能判断，这个语义必须被测到。

    ``confirms`` 从「启用 Gateway？」之后的问题开始算：那一问固定回答 True，
    否则每个用例都要在队首塞一个与被测行为无关的 True，一旦忘记就会静默地把
    整段配置关掉并提前返回——断言仍然「通过」，但什么都没测到。
    """
    def run(config=None, *, texts=(), confirms=(), lan="", mode_idx=1):
        text_answers = list(texts)
        confirm_answers = [True, *confirms]
        asked_texts: list[str] = []
        asked_confirms: list[str] = []

        def _text(message, default=""):
            asked_texts.append(message)
            answer = text_answers.pop(0) if text_answers else None
            return default if answer is None else (answer or default)

        def _confirm(message, default=True):
            asked_confirms.append(message)
            return confirm_answers.pop(0) if confirm_answers else default

        monkeypatch.setattr(wiz.ui, "text", _text)
        monkeypatch.setattr(wiz.ui, "confirm", _confirm)
        monkeypatch.setattr(wiz.ui, "password", lambda message: "s3cret")
        monkeypatch.setattr(wiz, "_choice", lambda q, labels, default=0: mode_idx)
        monkeypatch.setattr(wiz, "_print_section_header", lambda key: None)
        # 网络探测在单元测试里必须是确定的：真实调用会随运行机器的网卡而变。
        monkeypatch.setattr(wiz, "_primary_lan_address", lambda: lan)

        cfg = {"gateway": {"enabled": True}} if config is None else config
        wiz.setup_gateway(cfg)
        return SimpleNamespace(
            gateway=cfg["gateway"],
            auth=cfg["gateway"].get("auth", {}),
            texts=asked_texts,
            confirms=asked_confirms,
            out=capsys.readouterr().out,
        )

    return run


def test_wildcard_bind_never_prefills_the_bind_address(gateway_wizard):
    """0.0.0.0 一路回车不得写出 allowed_hosts=[0.0.0.0]。

    这是 7b54125 留下的主缺口：预填绑定地址让「按回车」产出一份浏览器永远匹配不到
    的 allowlist，同时因为列表非空而抑制了后续告警——用户以为修好了，实际没修。
    这里检测到 LAN 地址，回车应写入该地址。
    """
    result = gateway_wizard(texts=["0.0.0.0", "58123", None], lan="192.168.1.5")

    assert result.auth["allowed_hosts"] == ["192.168.1.5"]
    assert result.auth["allowed_origins"] == ["http://192.168.1.5:58123"]
    assert wiz._browser_unreachable_reason({"gateway": result.gateway}) == ""


def test_wildcard_bind_without_detectable_lan_warns_instead_of_writing_junk(gateway_wizard):
    """探不到 LAN 地址时留空：回车意味着「我知道，暂时不配」，而不是写入垃圾值。"""
    result = gateway_wizard(texts=["0.0.0.0", "58123", None], lan="")

    assert "allowed_hosts" not in result.auth
    assert "allowed_origins" not in result.auth
    assert wiz._browser_unreachable_reason({"gateway": result.gateway}) == "0.0.0.0"
    assert "allowed_hosts" in result.out


def test_user_entered_hosts_are_normalized_and_wildcards_dropped(gateway_wizard):
    """用户会直接从地址栏粘贴，带端口、带大写；也可能顺手填上绑定地址。

    GatewayAuth 比较的是规范化后的值，所以向导必须以同一形式落盘，否则配置看着
    对却匹配不到任何请求。
    """
    result = gateway_wizard(
        texts=["0.0.0.0", "58123", "Echo.Example.com, 192.168.1.5:58123, 0.0.0.0"],
    )

    assert result.auth["allowed_hosts"] == ["echo.example.com", "192.168.1.5"]
    assert result.auth["allowed_origins"] == [
        "http://echo.example.com:58123",
        "http://192.168.1.5:58123",
    ]


def test_reverse_proxy_origin_can_replace_direct_http_suggestion(gateway_wizard):
    result = gateway_wizard(
        texts=[
            "0.0.0.0",
            "58123",
            "Echo.Example.com",
            "https://Echo.Example.com:443/",
        ],
    )

    assert result.auth["allowed_hosts"] == ["echo.example.com"]
    assert result.auth["allowed_origins"] == ["https://echo.example.com"]


def test_saved_origin_allows_browser_without_fetch_metadata(gateway_wizard, tmp_path):
    """Close the exact production failure: reads worked, writes and WS 403'd."""
    from echo_agent.config.schema import GatewayAuthConfig
    from echo_agent.gateway.auth import GatewayAuth

    result = gateway_wizard(
        texts=["0.0.0.0", "58123", "123.56.188.16"],
    )
    auth = GatewayAuth(
        GatewayAuthConfig.model_validate(result.auth),
        tmp_path,
        bound_host=result.gateway["host"],
    )

    assert auth.is_cross_site_browser(
        "http://123.56.188.16:58123", "", "123.56.188.16:58123",
    ) is False
    assert auth.is_host_allowed("123.56.188.16:58123") is True


def test_saved_allowed_hosts_actually_match_real_host_headers(gateway_wizard, tmp_path):
    """端到端闭环：向导落盘的值必须让 GatewayAuth 真的放行浏览器发来的 Host。

    这是整条链路上唯一能证明「配完就能用」的断言——探测函数与文案都对，也不代表
    保存下来的值能通过 is_host_allowed。
    """
    from echo_agent.config.schema import GatewayAuthConfig
    from echo_agent.gateway.auth import GatewayAuth

    result = gateway_wizard(texts=["0.0.0.0", "58123", "echo.example.com"])

    auth = GatewayAuth(
        GatewayAuthConfig(mode="allowlist", api_tokens=["s3cret"],
                          allowed_hosts=result.auth["allowed_hosts"]),
        tmp_path, bound_host=result.gateway["host"],
    )
    assert auth.is_host_allowed("echo.example.com:58123")
    assert auth.is_host_allowed("ECHO.example.com")
    assert not auth.is_host_allowed("evil.example")


def test_loopback_bind_offers_to_clear_a_stale_allowlist(gateway_wizard):
    """从暴露态切回本机时，遗留的 allowlist 会把本机 Dashboard 锁在门外。

    显式 allowed_hosts 会覆盖默认的本机放行规则，而 _browser_unreachable_reason
    看到回环绑定就直接返回，什么都不会说。
    """
    cfg = {"gateway": {"enabled": True, "host": "0.0.0.0", "port": 58123,
                       "auth": {"mode": "allowlist", "api_tokens": ["t"],
                                "allowed_hosts": ["echo.example.com"]}}}

    result = gateway_wizard(cfg, texts=["127.0.0.1", "58123"], confirms=[True])

    assert "allowed_hosts" not in result.auth, "接受清空后不该留下遗留条目"


def test_declining_keeps_the_allowlist_but_normalizes_it(gateway_wizard):
    """反代场景是保留域名的正当理由；选「否」必须原样保住用户的配置。

    顺带收敛驼峰别名，避免 allowed_hosts / allowedHosts 两键并存互相打架。
    """
    cfg = {"gateway": {"enabled": True, "host": "0.0.0.0", "port": 58123,
                       "auth": {"mode": "allowlist", "api_tokens": ["t"],
                                "allowedHosts": ["Echo.Example.COM:443"]}}}

    result = gateway_wizard(cfg, texts=["127.0.0.1", "58123"], confirms=[False])

    assert result.auth["allowed_hosts"] == ["echo.example.com"]
    assert "allowedHosts" not in result.auth


def test_loopback_allowlist_that_already_covers_this_machine_is_left_alone(gateway_wizard):
    """已经含本机地址的 allowlist 没有问题，不该多问一句。"""
    cfg = {"gateway": {"enabled": True, "host": "127.0.0.1", "port": 58123,
                       "auth": {"mode": "allowlist", "api_tokens": ["t"],
                                "allowed_hosts": ["localhost", "echo.example.com"]}}}

    result = gateway_wizard(cfg, texts=["127.0.0.1", "58123"])

    assert result.auth["allowed_hosts"] == ["localhost", "echo.example.com"]
    assert not any("allowed_hosts" in q for q in result.confirms)


def test_loopback_bind_never_asks_for_an_allowlist(gateway_wizard):
    """全新的本机默认部署：空 allowed_hosts 本就默认放行本机，不该追问。"""
    result = gateway_wizard(texts=["127.0.0.1", "58123"])

    assert "allowed_hosts" not in result.auth
    assert not any("Host" in q or "allowed" in q.lower() for q in result.texts[2:])


def test_primary_lan_address_never_raises_and_never_suggests_a_wildcard(monkeypatch):
    """探测是尽力而为：离线或路由异常时必须返回 ""，而不是把向导带崩。"""
    import socket

    def _boom(*a, **kw):
        raise OSError("network unreachable")

    monkeypatch.setattr(socket, "socket", _boom)
    assert wiz._primary_lan_address() == ""


def test_primary_lan_address_rejects_loopback_results(monkeypatch):
    """某些环境下内核会给出 127.0.0.1 —— 那不是可供他人访问的地址，不该作建议值。"""
    class _Sock:
        def __enter__(self): return self
        def __exit__(self, *a): return False
        def settimeout(self, _): pass
        def connect(self, _): pass
        def getsockname(self): return ("127.0.0.1", 9)

    import socket
    monkeypatch.setattr(socket, "socket", lambda *a, **kw: _Sock())
    assert wiz._primary_lan_address() == ""
