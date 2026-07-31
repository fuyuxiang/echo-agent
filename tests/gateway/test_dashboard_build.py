"""Dashboard 按需构建。

安装期前置构建 pnpm 的失败模式很差：一行灰色 warn 埋在几百行输出中间，收尾却
无条件打印绿色成功横幅和 dashboard 链接，用户点开才发现 UI 是残的。改为使用时
按需构建（参照 hermes-agent），失败时机与失败后果对齐。
"""
import os
import time
from pathlib import Path

import pytest

from echo_agent.gateway import dashboard_build
from echo_agent.gateway.dashboard_build import (
    BuildReason,
    dashboard_build_needed,
    find_web_dir,
)


@pytest.fixture
def web(tmp_path):
    """一个最小的 web/ 源码树，带已构建产物。"""
    web_dir = tmp_path / "web"
    (web_dir / "src").mkdir(parents=True)
    (web_dir / "src" / "main.tsx").write_text("export {}", encoding="utf-8")
    (web_dir / "package.json").write_text("{}", encoding="utf-8")
    dist = web_dir / "dist"
    dist.mkdir()
    (dist / "index.html").write_text("<html></html>", encoding="utf-8")
    return web_dir


def test_missing_dist_needs_build(web):
    (web / "dist" / "index.html").unlink()
    assert dashboard_build_needed(web) is True


def test_fresh_dist_does_not_need_build(web):
    assert dashboard_build_needed(web) is False


def test_source_newer_than_dist_needs_build(web):
    src = web / "src" / "main.tsx"
    future = time.time() + 60
    os.utime(src, (future, future))
    assert dashboard_build_needed(web) is True


def test_lockfile_newer_than_dist_needs_build(web):
    lock = web / "pnpm-lock.yaml"
    lock.write_text("lockfileVersion: '9.0'", encoding="utf-8")
    future = time.time() + 60
    os.utime(lock, (future, future))
    assert dashboard_build_needed(web) is True


def test_node_modules_is_not_compared(web):
    """node_modules 里全是比 dist 新的文件，不能因此判定过期。"""
    nm = web / "node_modules" / "some-pkg"
    nm.mkdir(parents=True)
    stale_maker = nm / "index.js"
    stale_maker.write_text("module.exports = {}", encoding="utf-8")
    future = time.time() + 60
    os.utime(stale_maker, (future, future))
    assert dashboard_build_needed(web) is False


def test_find_web_dir_returns_none_for_wheel_install(monkeypatch, tmp_path):
    """wheel 安装没有 web/ 源码树，按需构建对其必须是 no-op。"""
    monkeypatch.setattr(dashboard_build, "_repo_root", lambda: tmp_path)
    assert find_web_dir() is None


def test_node_missing_asks_before_downloading(web, monkeypatch):
    """下载几十 MB 的 Node 运行时必须用户点头 —— 用户要的是「构建 Dashboard」，
    不是「下载一个 Node」。"""
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: None)
    asked = []
    outcome = dashboard_build.build_dashboard(
        web, confirm=lambda msg: asked.append(msg) or False,
    )
    assert asked, "should have asked before downloading Node"
    assert outcome.ok is False
    assert outcome.reason is BuildReason.NODE_DECLINED


def test_no_confirm_callback_never_downloads(web, monkeypatch):
    """非交互场景（后台服务）不能弹问也不能静默下载。"""
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: None)
    outcome = dashboard_build.build_dashboard(web, confirm=None)
    assert outcome.ok is False
    assert outcome.reason is BuildReason.NODE_MISSING


def test_pnpm_unavailable_is_reported_distinctly(web, monkeypatch):
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: None)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.reason is BuildReason.PNPM_UNAVAILABLE


def test_build_failure_keeps_stale_dist(web, monkeypatch):
    """陈旧的完整 UI 比退回简化页有用（hermes issue #23817 的经验）。"""
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_run_streamed", lambda *a, **kw: (1, "boom"))
    outcome = dashboard_build.build_dashboard(web)  # web fixture 已有 dist/index.html
    assert outcome.used_stale is True
    assert outcome.reason is BuildReason.STALE_KEPT


def test_build_failure_without_stale_reports_build_failed(web, monkeypatch):
    (web / "dist" / "index.html").unlink()
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_run_streamed", lambda *a, **kw: (1, "boom"))
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.ok is False
    assert outcome.used_stale is False
    assert outcome.reason in (BuildReason.INSTALL_FAILED, BuildReason.BUILD_FAILED)


def test_build_retries_once_before_giving_up(web, monkeypatch):
    """瞬时问题（杀毒扫描 Node 二进制、npm 缓存未就绪）重试一次即可。"""
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_sleep", lambda s: None)
    calls = []

    def flaky(cmd, cwd, **kw):
        calls.append(cmd)
        # install 成功；build 第一次失败、第二次成功。
        if "build" not in cmd:
            return 0, ""
        return (1, "transient") if len([c for c in calls if "build" in c]) == 1 else (0, "")

    monkeypatch.setattr(dashboard_build, "_run_streamed", flaky)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.ok is True
    assert len([c for c in calls if "build" in c]) == 2


def test_successful_build_reports_ok(web, monkeypatch):
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_run_streamed", lambda *a, **kw: (0, ""))
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.ok is True
    assert outcome.reason is BuildReason.OK
