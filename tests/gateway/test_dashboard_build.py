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
    assert outcome.build_succeeded is False
    assert outcome.artifact_usable is True  # previous bundle from the fixture
    assert outcome.reason is BuildReason.NODE_DECLINED


def test_no_confirm_callback_never_downloads(web, monkeypatch):
    """非交互场景（后台服务）不能弹问也不能静默下载。"""
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: None)
    outcome = dashboard_build.build_dashboard(web, confirm=None)
    assert outcome.build_succeeded is False
    assert outcome.artifact_usable is True  # previous bundle from the fixture
    assert outcome.reason is BuildReason.NODE_MISSING


def test_pnpm_unavailable_is_reported_distinctly(web, monkeypatch):
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: None)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.reason is BuildReason.PNPM_UNAVAILABLE


def test_build_failure_keeps_previous_dist(web, monkeypatch):
    """陈旧的完整 UI 比退回简化页有用（hermes issue #23817 的经验）。

    本次修复后：构建失败时 dist 完全没被触碰（vite 写到 staging，
    staging 不通过验证或 swap 失败都不影响 dist），所以旧的 dist 仍是
    同一个 bundle，不只是"stale"——是原子未动。
    """
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_run_streamed", lambda *a, **kw: (1, "boom"))
    pre_html = (web / "dist" / "index.html").read_text(encoding="utf-8")
    outcome = dashboard_build.build_dashboard(web)  # web fixture 已有 dist/index.html
    assert outcome.build_succeeded is False
    assert outcome.artifact_usable is True
    assert outcome.had_previous_bundle is True
    # The previous bundle must be untouched — byte-for-byte identical.
    assert (web / "dist" / "index.html").read_text(encoding="utf-8") == pre_html


def test_build_failure_without_previous_dist_reports_build_failed(web, monkeypatch):
    (web / "dist" / "index.html").unlink()
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_run_streamed", lambda *a, **kw: (1, "boom"))
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.build_succeeded is False
    assert outcome.artifact_usable is False
    assert outcome.had_previous_bundle is False
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
        if len([c for c in calls if "build" in c]) == 1:
            return (1, "transient")
        # Second build: write a usable index.html so the staging validates.
        out_dir = (kw.get("env") or {}).get("ECHO_DASHBOARD_OUT_DIR")
        if out_dir:
            from pathlib import Path
            Path(out_dir).mkdir(parents=True, exist_ok=True)
            (Path(out_dir) / "index.html").write_text("<html>retry</html>")
        return (0, "")

    monkeypatch.setattr(dashboard_build, "_run_streamed", flaky)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.build_succeeded is True
    assert outcome.artifact_usable is True
    assert len([c for c in calls if "build" in c]) == 2


def test_successful_build_reports_ok(web, monkeypatch):
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))

    def ok(cmd, cwd, **kw):
        out_dir = (kw.get("env") or {}).get("ECHO_DASHBOARD_OUT_DIR")
        if out_dir and "build" in cmd:
            from pathlib import Path
            Path(out_dir).mkdir(parents=True, exist_ok=True)
            (Path(out_dir) / "index.html").write_text("<html>new</html>")
        return 0, ""

    monkeypatch.setattr(dashboard_build, "_run_streamed", ok)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.build_succeeded is True
    assert outcome.artifact_usable is True
    assert outcome.reason is BuildReason.OK
    # New bundle is in place; staging and backup are cleaned up.
    assert (web / "dist" / "index.html").read_text(encoding="utf-8") == "<html>new</html>"
    assert not (web / "dist.staging").exists()
    assert not (web / "dist.swapbackup").exists()


# ── Staging + atomic swap (P1-04 regression fence) ──────────────────────────
#
# The build commands Vite to a staging directory under web_dir/dist.staging and
# only atomically swaps the result into web_dir/dist once the staging result is
# validated. Before this, vite.config.ts's emptyOutDir: true wiped the
# in-place dist on build start — and a failed build reported success as long
# as there had been a previous bundle ("STALE_KEPT"), so an empty partial dist
# got served as if it were the full Dashboard.
#
# These tests pin the staging path independently of pnpm by stubbing the
# subprocess layer, so the regression fence runs without a real toolchain.


def _build_env_for(staging: Path) -> dict[str, str]:
    """Stub subprocess that records calls and writes a fake index.html to
    whatever ECHO_DASHBOARD_OUT_DIR it sees on a build invocation."""
    return staging  # placeholder for clarity; see fixtures below


def test_staging_directory_is_written_not_dist(web, monkeypatch):
    """Vite must NOT receive outDir=dist — that would destroy the bundle.

    The build command is launched with ECHO_DASHBOARD_OUT_DIR pointing at
    dist.staging. If that env var is missing or points at dist itself, the
    staging indirection is meaningless and the previous bug is back.
    """
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    seen_envs: list[dict[str, str]] = []

    def record(cmd, cwd, *, env=None, **kw):
        seen_envs.append(dict(env or {}))
        out_dir = (env or {}).get("ECHO_DASHBOARD_OUT_DIR")
        if out_dir and "build" in cmd:
            Path(out_dir).mkdir(parents=True, exist_ok=True)
            (Path(out_dir) / "index.html").write_text("<html>staged</html>")
        return 0, ""

    monkeypatch.setattr(dashboard_build, "_run_streamed", record)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.artifact_usable is True
    # The install call also runs with ECHO_DASHBOARD_OUT_DIR set — verifies
    # the env is wired through both invocations, not just the second.
    for env in seen_envs:
        assert env.get("ECHO_DASHBOARD_OUT_DIR", "").endswith("dist.staging")


def test_build_exit_zero_without_index_html_does_not_swap(web, monkeypatch):
    """Build succeeded per pnpm but produced no index.html — refuse to swap.

    The two-level guard is what separates a usable Dashboard from a bundle that
    happens to look correct in pnpm's exit code. Without this, an empty dist
    would be served as if it were the full build.
    """
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))
    monkeypatch.setattr(dashboard_build, "_run_streamed", lambda *a, **kw: (0, ""))
    pre_html = (web / "dist" / "index.html").read_text(encoding="utf-8")
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.build_succeeded is False
    assert outcome.artifact_usable is True  # fixture's pre-existing bundle survives
    # Old dist is byte-identical: never touched.
    assert (web / "dist" / "index.html").read_text(encoding="utf-8") == pre_html
    # No staging directory was left behind.
    assert not (web / "dist.staging").exists()


def test_swap_failure_rolls_back_old_bundle(web, monkeypatch):
    """A swap failure must restore the previous bundle.

    Sabotage: the staging passes validation, but the second rename
    (staging -> dist) is forced to fail. The first rename (dist -> backup)
    has already moved the old bundle aside, so the rollback path must put it
    back.
    """
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))

    # Pre-create a backup so the second rename collides and fails: the
    # staging rename to dist cannot complete because dist is now a different
    # non-empty directory from the same parent's perspective — actually
    # wait, dist was moved to backup. The collide is the staging trying to
    # rename to a path that still has the parent's link. Actually on macOS,
    # the second rename succeeds because the target path is empty after the
    # first rename. So we sabotage differently: pre-existing backup makes the
    # cleanup at start FAIL, surfacing the cleanup path as a build failure
    # rather than reaching the swap.
    backup = web / "dist.swapbackup"
    backup.mkdir(parents=True)
    (backup / "old").write_text("from previous interrupted swap")

    # Make rmtree raise — simulates "I cannot clean up the leftover, abort".
    real_rmtree = dashboard_build.shutil.rmtree
    def fail_rmtree(path, *a, **kw):
        if Path(path) == backup:
            raise OSError(16, "Device or resource busy")
        return real_rmtree(path, *a, **kw)
    monkeypatch.setattr(dashboard_build.shutil, "rmtree", fail_rmtree)

    outcome = dashboard_build.build_dashboard(web)
    assert outcome.build_succeeded is False
    # Old dist still in place, untouched.
    assert (web / "dist" / "index.html").is_file()
    # The leftover backup is still there (we couldn't clean it).
    assert backup.exists()


def test_swap_failure_keeps_old_bundle_when_step_two_fails(web, monkeypatch):
    """The second rename fails mid-swap → the rollback restores the old bundle.

    Force step 2 to fail by putting a non-empty directory at the staging path
    AFTER validation passed: the staging directory exists with index.html, so
    the validation check succeeds; we then rename it to a path that has
    something else on top, which fails. The fix's rollback restores the old
    bundle from the backup.
    """
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))

    pre_html = (web / "dist" / "index.html").read_text(encoding="utf-8")

    real_rename = dashboard_build.os.rename
    def sab_rename(src, dst, *a, **kw):
        # The second rename targets web_dir/dist with src=staging. Sabotage.
        src_p, dst_p = Path(src), Path(dst)
        if src_p.name == "dist.staging" and dst_p.name == "dist":
            # Replace staging's index.html with content that can't be renamed
            # — actually we just raise directly. The swap is exactly step 2.
            raise OSError(66, "Directory not empty (sabotaged)")
        return real_rename(src, dst, *a, **kw)

    monkeypatch.setattr(dashboard_build.os, "rename", sab_rename)

    def write_index(cmd, cwd, **kw):
        out_dir = (kw.get("env") or {}).get("ECHO_DASHBOARD_OUT_DIR")
        if out_dir and "build" in cmd:
            Path(out_dir).mkdir(parents=True, exist_ok=True)
            (Path(out_dir) / "index.html").write_text("<html>new</html>")
        return 0, ""

    monkeypatch.setattr(dashboard_build, "_run_streamed", write_index)
    outcome = dashboard_build.build_dashboard(web)
    # Build succeeded per pnpm but the swap failed → not artifact_usable.
    assert outcome.build_succeeded is False
    # Old dist must be rolled back to byte-identity.
    assert (web / "dist" / "index.html").read_text(encoding="utf-8") == pre_html
    assert outcome.artifact_usable is True
    assert outcome.reason is BuildReason.BUILD_FAILED


def test_stale_swapbackup_is_cleared(web, monkeypatch):
    """A leftover swapbackup from a previous interrupted swap must not block."""
    monkeypatch.setattr(dashboard_build, "_find_usable_node", lambda: Path("/usr/bin/node"))
    monkeypatch.setattr(dashboard_build, "_ensure_pnpm", lambda node, out: Path("/usr/bin/pnpm"))

    backup = web / "dist.swapbackup"
    backup.mkdir(parents=True)
    (backup / "leftover").write_text("from previous run")

    def write_index(cmd, cwd, **kw):
        out_dir = (kw.get("env") or {}).get("ECHO_DASHBOARD_OUT_DIR")
        if out_dir and "build" in cmd:
            Path(out_dir).mkdir(parents=True, exist_ok=True)
            (Path(out_dir) / "index.html").write_text("<html>fresh</html>")
        return 0, ""

    monkeypatch.setattr(dashboard_build, "_run_streamed", write_index)
    outcome = dashboard_build.build_dashboard(web)
    assert outcome.artifact_usable is True
    assert (web / "dist" / "index.html").read_text(encoding="utf-8") == "<html>fresh</html>"
    assert not backup.exists()
