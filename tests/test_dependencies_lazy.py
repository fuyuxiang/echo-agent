"""Tests for echo_agent.dependencies — lazy_deps, cli, skill_require.

Covers spec safety validation, version checks, the public API, the install
engine (with subprocess fully mocked), the deps CLI dispatch, and the
skill_require convenience helpers.
"""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import pytest

from echo_agent.dependencies import lazy_deps as ld


# ── _spec_is_safe ─────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "spec",
    [
        "openpyxl",
        "openpyxl>=3.1",
        "duckduckgo_search>=7.0",
        "google-generativeai>=0.8",
        "uvicorn[standard]>=0.30",
        "pkg>=1.0,<2.0",
    ],
)
def test_spec_is_safe_accepts_valid(spec):
    assert ld._spec_is_safe(spec) is True


@pytest.mark.parametrize(
    "spec",
    [
        "",
        "x" * 201,                       # too long
        "pkg; rm -rf /",                 # shell metachar
        "pkg | cat",
        "pkg && evil",
        "pkg`whoami`",
        "pkg$VAR",
        "pkg\nmore",
        "pkg\\path",
        "-rrequirements.txt",            # starts with dash
        "/abs/path",                     # starts with slash
        "./rel/path",                    # starts with dot
        "git+https://example.com/x",     # URL
        "pkg @ file:///tmp/x",           # @ direct ref
        "https://example.com/x.whl",
    ],
)
def test_spec_is_safe_rejects_dangerous(spec):
    assert ld._spec_is_safe(spec) is False


# ── name / specifier extraction ──────────────────────────────────────────────


def test_pkg_name_from_spec():
    assert ld._pkg_name_from_spec("openpyxl>=3.1") == "openpyxl"
    assert ld._pkg_name_from_spec("google-generativeai>=0.8") == "google-generativeai"
    assert ld._pkg_name_from_spec("plain") == "plain"


def test_specifier_from_spec():
    assert ld._specifier_from_spec("openpyxl>=3.1") == ">=3.1"
    assert ld._specifier_from_spec("openpyxl") == ""
    assert ld._specifier_from_spec("uvicorn[standard]>=0.30") == ">=0.30"


# ── _is_satisfied / _is_present ──────────────────────────────────────────────


def test_is_satisfied_missing_package():
    with patch("importlib.metadata.version", side_effect=__import__("importlib.metadata", fromlist=["PackageNotFoundError"]).PackageNotFoundError):
        assert ld._is_satisfied("definitely-not-installed-xyz>=1.0") is False


def test_is_satisfied_no_specifier_present():
    with patch("importlib.metadata.version", return_value="1.2.3"):
        assert ld._is_satisfied("somepkg") is True


def test_is_satisfied_version_in_range():
    with patch("importlib.metadata.version", return_value="3.5.0"):
        assert ld._is_satisfied("openpyxl>=3.1") is True


def test_is_satisfied_version_out_of_range():
    with patch("importlib.metadata.version", return_value="2.0.0"):
        assert ld._is_satisfied("openpyxl>=3.1") is False


def test_is_satisfied_unparseable_version_is_lenient():
    # Non-PEP440 installed version → treated as satisfied rather than blocking.
    with patch("importlib.metadata.version", return_value="weird-version"):
        assert ld._is_satisfied("openpyxl>=3.1") is True


def test_is_present_true_and_false():
    with patch("importlib.metadata.version", return_value="1.0"):
        assert ld._is_present("anything") is True
    from importlib.metadata import PackageNotFoundError
    with patch("importlib.metadata.version", side_effect=PackageNotFoundError):
        assert ld._is_present("nope") is False


# ── feature_* public API ─────────────────────────────────────────────────────


def test_feature_specs_known_and_unknown():
    assert ld.feature_specs("skill.excel-author") == ("openpyxl>=3.1",)
    with pytest.raises(KeyError):
        ld.feature_specs("skill.does-not-exist")


def test_feature_install_command():
    cmd = ld.feature_install_command("skill.excel-author")
    assert cmd is not None and "uv pip install" in cmd and "openpyxl" in cmd
    # empty-spec feature and unknown feature both yield None
    assert ld.feature_install_command("skill.calculator") is None
    assert ld.feature_install_command("unknown") is None


def test_is_available_empty_specs_always_true():
    assert ld.is_available("skill.calculator") is True


def test_is_available_unknown_feature_false():
    assert ld.is_available("skill.unknown-xyz") is False


def test_is_available_reflects_missing(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    assert ld.is_available("skill.excel-author") is False
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: True)
    assert ld.is_available("skill.excel-author") is True


def test_feature_missing(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    assert ld.feature_missing("skill.excel-author") == ("openpyxl>=3.1",)


# ── FeatureUnavailable formatting ────────────────────────────────────────────


def test_feature_unavailable_message_with_missing():
    err = ld.FeatureUnavailable("skill.x", ("pkg>=1.0",), "boom")
    text = str(err)
    assert "skill.x" in text and "boom" in text and "pkg>=1.0" in text
    assert err.feature == "skill.x" and err.reason == "boom"


def test_feature_unavailable_message_without_missing():
    err = ld.FeatureUnavailable("skill.x", (), "boom")
    assert "boom" in str(err)


# ── _allow_lazy_installs ─────────────────────────────────────────────────────


def test_allow_lazy_installs_env_disable(monkeypatch):
    monkeypatch.setenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", "1")
    assert ld._allow_lazy_installs() is False


def test_allow_lazy_installs_config_false(monkeypatch):
    monkeypatch.delenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", raising=False)
    fake_cfg = MagicMock()
    fake_cfg.skills.allow_lazy_installs = False
    with patch("echo_agent.config.loader.load_config", return_value=fake_cfg):
        assert ld._allow_lazy_installs() is False


def test_allow_lazy_installs_default_true(monkeypatch):
    monkeypatch.delenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", raising=False)
    with patch("echo_agent.config.loader.load_config", side_effect=RuntimeError):
        assert ld._allow_lazy_installs() is True


# ── ensure() decision paths ──────────────────────────────────────────────────


def test_ensure_unknown_feature_raises():
    with pytest.raises(ld.FeatureUnavailable):
        ld.ensure("skill.unknown-xyz", prompt=False)


def test_ensure_empty_specs_noop():
    # Should return without touching install machinery.
    ld.ensure("skill.calculator", prompt=False)


def test_ensure_already_satisfied_noop(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: True)
    ld.ensure("skill.excel-author", prompt=False)


def test_ensure_rejects_unsafe_spec(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setitem(ld.SKILL_DEPS, "skill._evil", ("pkg; rm -rf /",))
    try:
        with pytest.raises(ld.FeatureUnavailable, match="unsafe spec"):
            ld.ensure("skill._evil", prompt=False)
    finally:
        del ld.SKILL_DEPS["skill._evil"]


def test_ensure_disabled_raises(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: False)
    with pytest.raises(ld.FeatureUnavailable, match="disabled"):
        ld.ensure("skill.excel-author", prompt=False)


def test_ensure_install_success(monkeypatch):
    # First check: missing. After install: satisfied.
    calls = {"n": 0}

    def fake_satisfied(spec):
        calls["n"] += 1
        return calls["n"] > 1  # missing first, satisfied after install

    monkeypatch.setattr(ld, "_is_satisfied", fake_satisfied)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(
        ld, "_venv_pip_install", lambda specs, **kw: ld._InstallResult(True, "ok", "")
    )
    ld.ensure("skill.excel-author", prompt=False)


def test_ensure_install_failure_raises(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(
        ld, "_venv_pip_install",
        lambda specs, **kw: ld._InstallResult(False, "", "pip exploded"),
    )
    with pytest.raises(ld.FeatureUnavailable, match="install failed"):
        ld.ensure("skill.excel-author", prompt=False)


def test_ensure_install_succeeds_but_still_missing(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(
        ld, "_venv_pip_install", lambda specs, **kw: ld._InstallResult(True, "ok", "")
    )
    with pytest.raises(ld.FeatureUnavailable, match="still not importable"):
        ld.ensure("skill.excel-author", prompt=False)


def test_ensure_prompt_decline(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
    monkeypatch.setattr(ld.sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(ld.sys.stdout, "isatty", lambda: True, raising=False)
    with patch("builtins.input", return_value="n"):
        with pytest.raises(ld.FeatureUnavailable, match="declined"):
            ld.ensure("skill.excel-author", prompt=True)


# ── _venv_pip_install engine ─────────────────────────────────────────────────


def test_venv_pip_install_empty_specs():
    r = ld._venv_pip_install(())
    assert r.success is True


def test_venv_pip_install_uv_success(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: "/usr/bin/uv")
    proc = MagicMock(returncode=0, stdout="installed", stderr="")
    with patch.object(ld, "run_owned", return_value=proc) as run:
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is True
    # uv path should be the first call
    assert run.call_args_list[0].args[0][0] == "/usr/bin/uv"


def test_venv_pip_install_falls_back_to_pip(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: None)  # no uv

    def fake_run(cmd, **kw):
        if "--version" in cmd:
            return MagicMock(returncode=0, stdout="pip 24", stderr="")
        return MagicMock(returncode=0, stdout="done", stderr="")

    with patch.object(ld, "run_owned", side_effect=fake_run):
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is True


def test_venv_pip_install_pip_failure(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: None)

    def fake_run(cmd, **kw):
        if "--version" in cmd:
            return MagicMock(returncode=0, stdout="pip", stderr="")
        return MagicMock(returncode=1, stdout="", stderr="resolution impossible")

    with patch.object(ld, "run_owned", side_effect=fake_run):
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is False
    assert "resolution impossible" in r.stderr


def test_venv_pip_install_timeout(monkeypatch):
    monkeypatch.setattr(ld.shutil, "which", lambda name: None)

    def fake_run(cmd, **kw):
        if "--version" in cmd:
            return MagicMock(returncode=0, stdout="pip", stderr="")
        raise ld.subprocess.TimeoutExpired(cmd, 1)

    with patch.object(ld, "run_owned", side_effect=fake_run):
        r = ld._venv_pip_install(("openpyxl>=3.1",))
    assert r.success is False
    assert "timed out" in r.stderr


# ── active_features / refresh / check_all ────────────────────────────────────


def test_active_features(monkeypatch):
    monkeypatch.setattr(ld, "_is_present", lambda spec: "openpyxl" in spec)
    active = ld.active_features()
    assert "skill.excel-author" in active
    assert "skill.calculator" not in active  # empty specs are skipped


def test_check_all_features_shape(monkeypatch):
    monkeypatch.setattr(ld, "_is_satisfied", lambda spec: False)
    report = ld.check_all_features()
    # empty-spec feature is always available with no command
    assert report["skill.calculator"]["available"] is True
    assert report["skill.calculator"]["command"] is None
    # excel needs openpyxl which we forced missing
    assert report["skill.excel-author"]["available"] is False
    assert report["skill.excel-author"]["missing"] == ["openpyxl>=3.1"]
    assert report["skill.excel-author"]["command"] is not None


def test_refresh_active_features(monkeypatch):
    monkeypatch.setattr(ld, "active_features", lambda: ["skill.excel-author", "skill.tts-voice"])

    def fake_missing(feature):
        return () if feature == "skill.excel-author" else ("edge-tts>=7.0",)

    monkeypatch.setattr(ld, "feature_missing", fake_missing)
    monkeypatch.setattr(ld, "ensure", lambda feature, prompt=False: None)
    results = ld.refresh_active_features(prompt=False)
    assert results["skill.excel-author"] == "current"
    assert results["skill.tts-voice"] == "refreshed"


def test_refresh_active_features_failure(monkeypatch):
    monkeypatch.setattr(ld, "active_features", lambda: ["skill.tts-voice"])
    monkeypatch.setattr(ld, "feature_missing", lambda f: ("edge-tts>=7.0",))

    def boom(feature, prompt=False):
        raise ld.FeatureUnavailable(feature, ("edge-tts>=7.0",), "install failed: x")

    monkeypatch.setattr(ld, "ensure", boom)
    results = ld.refresh_active_features(prompt=False)
    assert results["skill.tts-voice"].startswith("failed")


def test_refresh_active_features_skipped(monkeypatch):
    monkeypatch.setattr(ld, "active_features", lambda: ["skill.tts-voice"])
    monkeypatch.setattr(ld, "feature_missing", lambda f: ("edge-tts>=7.0",))

    def declined(feature, prompt=False):
        raise ld.FeatureUnavailable(feature, ("edge-tts>=7.0",), "user declined install")

    monkeypatch.setattr(ld, "ensure", declined)
    results = ld.refresh_active_features(prompt=False)
    assert results["skill.tts-voice"].startswith("skipped")


# ── install_authorized (out-of-band authorized installs) ─────────────────────


class TestInstallAuthorized:
    def test_unsafe_spec_rejected_without_install(self, monkeypatch):
        called = {"n": 0}
        monkeypatch.setattr(ld, "_venv_pip_install",
                            lambda specs, **kw: called.__setitem__("n", called["n"] + 1))
        out = ld.install_authorized(("evil; rm -rf /",), source="test")
        assert out["success"] is False
        assert out["rejected"] == ["evil; rm -rf /"]
        assert called["n"] == 0  # 不安全 spec 绝不触发安装

    def test_already_satisfied_skips(self, monkeypatch):
        monkeypatch.setattr(ld, "_is_satisfied", lambda s: True)
        out = ld.install_authorized(("python-pptx",), source="test")
        assert out["success"] is True
        assert out["skipped"] == ["python-pptx"]
        assert out["installed"] == []

    def test_install_success_ignores_allowlist_and_switch(self, monkeypatch):
        # 关键:白名单外的包 + 开关关闭,也应安装(授权即同意)
        monkeypatch.setenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", "1")
        calls = {"n": 0}
        def fake_satisfied(spec):
            calls["n"] += 1
            return calls["n"] > 1  # 装前 missing,装后 satisfied
        monkeypatch.setattr(ld, "_is_satisfied", fake_satisfied)
        monkeypatch.setattr(ld, "_venv_pip_install",
                            lambda specs, **kw: ld._InstallResult(True, "ok", ""))
        out = ld.install_authorized(("some-third-party-pkg",), source="test")
        assert out["success"] is True
        assert out["installed"] == ["some-third-party-pkg"]

    def test_install_failure_returns_detail(self, monkeypatch):
        monkeypatch.setattr(ld, "_is_satisfied", lambda s: False)
        monkeypatch.setattr(ld, "_venv_pip_install",
                            lambda specs, **kw: ld._InstallResult(False, "", "pip exploded"))
        out = ld.install_authorized(("pkg",), source="test")
        assert out["success"] is False
        assert "pip exploded" in out["detail"]


class TestAsyncWrappersDoNotBlockLoop:
    """ensure_async / install_authorized_async 必须把同步 pip 安装丢到线程,
    否则一次离线安装(最长 300s)会冻住事件循环,触发看门狗杀进程重启循环。"""

    @pytest.mark.asyncio
    async def test_ensure_async_keeps_loop_responsive(self, monkeypatch):
        import asyncio
        import threading

        install_started = threading.Event()
        release_install = threading.Event()
        install_thread: dict[str, int] = {}
        loop_thread_id = threading.get_ident()

        # 模拟一次"慢"安装:同步阻塞直到测试放行。记录它实际运行的线程 ID,
        # 用于直接断言它没有跑在事件循环线程上(否则退化成同步阻塞也能靠
        # "安装前已积累的心跳数 > 0"蒙混过关)。
        def blocking_install(specs, **kw):
            install_thread["id"] = threading.get_ident()
            install_started.set()
            release_install.wait(timeout=5)
            return ld._InstallResult(True, "ok", "")

        # 安装前缺失、安装后满足:blocking_install 会 set install_started,
        # 之后的 feature_missing 校验才返回空(否则 ensure 判"装完仍缺"报错)。
        monkeypatch.setattr(ld, "feature_missing",
                            lambda f: [] if install_started.is_set() else ["pkg==1.0"])
        monkeypatch.setattr(ld, "_spec_is_safe", lambda s: True)
        monkeypatch.setattr(ld, "_allow_lazy_installs", lambda: True)
        monkeypatch.setattr(ld, "_venv_pip_install", blocking_install)
        monkeypatch.setattr(ld, "SKILL_DEPS", {"skill.x": ("pkg==1.0",)})

        heartbeats = 0

        async def heartbeat():
            nonlocal heartbeats
            while not release_install.is_set():
                heartbeats += 1
                await asyncio.sleep(0.005)

        hb = asyncio.create_task(heartbeat())
        ensure_task = asyncio.create_task(ld.ensure_async("skill.x", prompt=False))

        # 等安装在线程里真正开始。用忙等而非固定 sleep,避免依赖调度时序。
        for _ in range(200):
            if install_started.is_set():
                break
            await asyncio.sleep(0.005)
        assert install_started.is_set(), "install should have started in a worker thread"

        # 关键断言一:安装必须运行在非事件循环线程上。若实现退化成
        # 直接同步调用,install_thread["id"] 会等于 loop_thread_id。
        assert install_thread["id"] != loop_thread_id, \
            "install must run off the event-loop thread"

        # 关键断言二:以"安装开始那一刻"为基线,验证安装仍在阻塞期间心跳
        # 计数继续增长——证明循环在安装进行中依旧可推进,而不是靠安装前
        # 积累的旧计数。
        baseline = heartbeats
        for _ in range(40):
            if heartbeats > baseline:
                break
            await asyncio.sleep(0.005)
        assert heartbeats > baseline, \
            "event loop was frozen while the install was still blocking"

        release_install.set()
        await ensure_task
        hb.cancel()

    @pytest.mark.asyncio
    async def test_ensure_async_serializes_concurrent_installs(self, monkeypatch):
        """并发的两次安装必须串行化,不能同时写同一个 site-packages。"""
        import asyncio
        import threading

        concurrency = {"cur": 0, "max": 0}
        lock = threading.Lock()

        def blocking_install(specs, **kw):
            with lock:
                concurrency["cur"] += 1
                concurrency["max"] = max(concurrency["max"], concurrency["cur"])
            # 停留一会儿制造重叠窗口;若未串行化,max 会到 2。
            release = threading.Event()
            release.wait(timeout=0.1)
            with lock:
                concurrency["cur"] -= 1
            return ld._InstallResult(True, "ok", "")

        # 每个 spec 首次查询未安装(触发安装),之后视为已安装(装后校验通过)。
        seen: set[str] = set()
        seen_guard = threading.Lock()

        def fake_satisfied(spec):
            with seen_guard:
                if spec in seen:
                    return True
                seen.add(spec)
                return False

        monkeypatch.setattr(ld, "_spec_is_safe", lambda s: True)
        monkeypatch.setattr(ld, "_is_satisfied", fake_satisfied)
        monkeypatch.setattr(ld, "_venv_pip_install", blocking_install)

        await asyncio.gather(
            ld.install_authorized_async(("pkg-a",), source="t1"),
            ld.install_authorized_async(("pkg-b",), source="t2"),
        )
        assert concurrency["max"] == 1, \
            "concurrent installs must be serialized on the shared venv"

    @pytest.mark.asyncio
    async def test_install_authorized_async_delegates(self, monkeypatch):
        # 装前 missing、装后 satisfied(install_authorized 会二次校验可导入性)。
        calls = {"n": 0}
        def fake_satisfied(spec):
            calls["n"] += 1
            return calls["n"] > 1
        monkeypatch.setattr(ld, "_is_satisfied", fake_satisfied)
        monkeypatch.setattr(ld, "_venv_pip_install",
                            lambda specs, **kw: ld._InstallResult(True, "ok", ""))
        out = await ld.install_authorized_async(("pkg",), source="test")
        assert out["success"] is True
        assert out["installed"] == ["pkg"]
