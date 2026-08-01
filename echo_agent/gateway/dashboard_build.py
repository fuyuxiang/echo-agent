"""Build the Dashboard SPA on demand, rather than during installation.

install.sh used to build the frontend up front. That made a working pnpm/Node
toolchain a prerequisite of *installing an agent*, and when it failed the user
got one grey warn line mid-scroll followed by a green "Installation Complete"
banner and a dashboard URL that served a stripped-down playground instead. The
installer was promising something it already knew was untrue.

Building here instead means the failure lands the moment the user asks for the
Dashboard — when they are present, their intent is unambiguous, and the
diagnosis is worth reading. It also keeps the artifact matched to the checked-out
code, which fetching a pre-built bundle from a release would not.

`pip install echo-agent` users are unaffected: hatch_build.py bundles web/dist
into the wheel, GatewayServer._resolve_dashboard_dir finds it first, and
find_web_dir() returns None here so nothing in this module runs.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Callable

# base.py only imports stdlib (its echo_home is deferred), so this does not
# create a gateway -> cli import cycle.
from echo_agent.cli.service.base import GATEWAY_ENV_FLAG

# nodejs floor. Matches install.sh:node_version_ok, which accepts >= 20.
MIN_NODE_MAJOR = 20
# pnpm major to fall back to when corepack is unavailable. Keep in sync with
# install.sh:PNPM_FALLBACK_VERSION and web/package.json's "packageManager":
# a plain `npm i -g pnpm` grabs the newest major, and pnpm 11 needs Node
# >= 22.13 while MIN_NODE_MAJOR is 20 — that combination installs cleanly and
# then dies on every call with ERR_UNKNOWN_BUILTIN_MODULE: node:sqlite.
PNPM_FALLBACK_MAJOR = "10"

_SOURCE_EXTENSIONS = (".ts", ".tsx", ".js", ".jsx", ".css", ".html", ".vue")
_META_FILES = ("package.json", "pnpm-lock.yaml", "vite.config.ts", "vite.config.js")
_SKIP_DIRS = frozenset({"node_modules", "dist"})


class BuildReason(str, Enum):
    OK = "ok"
    NOT_A_SOURCE_CHECKOUT = "not_a_source_checkout"
    NODE_MISSING = "node_missing"
    NODE_TOO_OLD = "node_too_old"
    NODE_DECLINED = "node_declined"
    PNPM_UNAVAILABLE = "pnpm_unavailable"
    INSTALL_FAILED = "install_failed"
    BUILD_FAILED = "build_failed"


@dataclass(frozen=True)
class BuildOutcome:
    # build_succeeded: the build command ran and reported success. Independent
    # of whether any artifact is in place to serve — a missing or broken
    # artifact must surface as its own failure, not be papered over with "the
    # old bundle is still there". Callers wanting to know whether anything
    # usable is in ``web/dist`` after the call should check artifact_usable,
    # not ok.
    build_succeeded: bool
    # artifact_usable: a verified, atomically-installed bundle is in ``web/dist``
    # — build succeeded AND the staging result passed validation AND the swap
    # completed. False on any failure path; the caller can fall back to a
    # placeholder or report an error.
    artifact_usable: bool
    reason: BuildReason
    detail: str = ""
    # Whether an older bundle was already on disk before this call. Useful for
    # diagnostics; does NOT change artifact_usable — that field is about the
    # post-call state, not the history.
    had_previous_bundle: bool = False


def _repo_root() -> Path:
    """Repo root for a source checkout: .../echo_agent/gateway/ -> two up."""
    return Path(__file__).resolve().parent.parent.parent


def find_web_dir() -> Path | None:
    """The frontend source tree, or None when this is not a source checkout.

    None means a wheel install (the SPA is already bundled) — callers must treat
    that as "nothing to do", never as an error.
    """
    web = _repo_root() / "web"
    return web if (web / "package.json").is_file() else None


def dashboard_build_needed(web_dir: Path) -> bool:
    """Whether web/dist is missing or older than the sources that produce it.

    mtime rather than a content hash: `git pull` can touch mtimes without
    changing content, so this over-triggers occasionally. A dashboard build is
    cheap enough that one spurious rebuild beats hashing the whole source tree
    on every gateway start.
    """
    sentinel = web_dir / "dist" / "index.html"
    if not sentinel.is_file():
        return True
    try:
        dist_mtime = sentinel.stat().st_mtime
    except OSError:
        return True

    for meta in _META_FILES:
        path = web_dir / meta
        try:
            if path.is_file() and path.stat().st_mtime > dist_mtime:
                return True
        except OSError:
            continue

    for dirpath, dirnames, filenames in os.walk(web_dir, topdown=True):
        dirnames[:] = [d for d in dirnames if d not in _SKIP_DIRS]
        for name in filenames:
            if not name.endswith(_SOURCE_EXTENSIONS):
                continue
            try:
                if os.path.getmtime(os.path.join(dirpath, name)) > dist_mtime:
                    return True
            except OSError:
                continue
    return False


def _sleep(seconds: float) -> None:
    time.sleep(seconds)


def _node_major(node: Path) -> int:
    try:
        out = subprocess.run(
            [str(node), "--version"], capture_output=True, text=True, timeout=15,
        ).stdout.strip().lstrip("v")
    except (OSError, subprocess.SubprocessError):
        return 0
    head = out.split(".", 1)[0]
    return int(head) if head.isdigit() else 0


def _managed_node() -> Path:
    from echo_agent.runtime_paths import echo_home

    return echo_home() / "node" / "bin" / "node"


def _find_usable_node() -> Path | None:
    """System Node first, then the one install.sh may have managed under
    ~/.echo-agent/node. Returns None when neither satisfies MIN_NODE_MAJOR."""
    system = shutil.which("node")
    if system and _node_major(Path(system)) >= MIN_NODE_MAJOR:
        return Path(system)
    managed = _managed_node()
    if managed.is_file() and _node_major(managed) >= MIN_NODE_MAJOR:
        return managed
    return None


def _download_node(on_output: Callable[[str], None] | None) -> Path | None:
    """Delegate to install.sh's Node installer.

    Deliberately not reimplemented in Python: that bash path verifies the
    official SHASUMS256.txt, races three mirrors, refuses musl (no official
    build exists) and maps architectures. Re-deriving all of it here would be
    pure regression risk for no gain.
    """
    script = _repo_root() / "scripts" / "install.sh"
    if not script.is_file():
        return None
    rc, _ = _run_streamed(
        ["bash", str(script), "--install-node-only"], cwd=_repo_root(),
        on_output=on_output,
    )
    if rc != 0:
        return None
    managed = _managed_node()
    return managed if managed.is_file() and _node_major(managed) >= MIN_NODE_MAJOR else None


def _run_streamed(
    cmd: list[str],
    cwd: Path,
    *,
    on_output: Callable[[str], None] | None = None,
    idle_timeout: float = 180.0,
    env: dict[str, str] | None = None,
) -> tuple[int, str]:
    """Run a command, streaming output, killing it only when output goes idle.

    Not a fixed wall-clock timeout: `pnpm build` on a slow or memory-starved
    host (WSL2 defaults to a 4 GB cap) can legitimately take many minutes, and
    the old fixed 600s ceiling in install.sh both killed slow-but-healthy builds
    and made a genuinely hung one wait the full ten minutes. Idle output is the
    signal that actually distinguishes the two. Streaming matters as much: a
    silent capture_output build is indistinguishable from a hang, and users
    react by rebooting mid-install.
    """
    chunks: list[str] = []
    last = time.monotonic()
    lock = threading.Lock()

    try:
        proc = subprocess.Popen(
            cmd, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace", bufsize=1, env=env,
        )
    except OSError as exc:
        return 127, str(exc)

    def reader() -> None:
        nonlocal last
        assert proc.stdout is not None
        for line in proc.stdout:
            if on_output is not None:
                on_output(line.rstrip())
            with lock:
                chunks.append(line)
                last = time.monotonic()

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()

    killed = False
    while True:
        try:
            rc = proc.wait(timeout=5)
            break
        except subprocess.TimeoutExpired:
            with lock:
                idle = time.monotonic() - last
            if idle > idle_timeout:
                killed = True
                proc.terminate()
                try:
                    rc = proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    rc = proc.wait()
                break

    thread.join(timeout=2)
    output = "".join(chunks)
    if killed:
        output += f"\n(no output for {idle_timeout:.0f}s — terminated)"
        return rc or 1, output
    return rc, output


def _ensure_pnpm(node: Path, on_output: Callable[[str], None] | None) -> Path | None:
    """Locate a pnpm that actually RUNS, bootstrapping one if needed.

    Probing with `pnpm --version` rather than `which pnpm` is deliberate: an
    installed-but-unrunnable pnpm is a real state. An earlier installer left
    pnpm 11 on hosts with Node 20, where every invocation dies with
    ERR_UNKNOWN_BUILTIN_MODULE: node:sqlite. corepack is tried first because it
    honours web/package.json's "packageManager" pin and so lands on the version
    CI tests; `npm i -g pnpm@10` is the fallback, pinned for the same reason.
    """
    bin_dir = node.parent
    env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"}

    existing = shutil.which("pnpm", path=env["PATH"])
    if existing:
        rc, _ = _run_streamed([existing, "--version"], cwd=bin_dir, idle_timeout=60, env=env)
        if rc == 0:
            return Path(existing)
        if on_output is not None:
            on_output("pnpm is on PATH but does not run; re-bootstrapping it.")

    corepack = shutil.which("corepack", path=env["PATH"])
    if corepack:
        _run_streamed([corepack, "enable", "pnpm"], cwd=bin_dir, idle_timeout=120, env=env)
        found = shutil.which("pnpm", path=env["PATH"])
        if found:
            rc, _ = _run_streamed([found, "--version"], cwd=bin_dir, idle_timeout=60, env=env)
            if rc == 0:
                return Path(found)

    npm = shutil.which("npm", path=env["PATH"])
    if npm:
        _run_streamed(
            [npm, "install", "-g", f"pnpm@{PNPM_FALLBACK_MAJOR}"],
            cwd=bin_dir, idle_timeout=300, env=env, on_output=on_output,
        )
        found = shutil.which("pnpm", path=env["PATH"])
        if found:
            rc, _ = _run_streamed([found, "--version"], cwd=bin_dir, idle_timeout=60, env=env)
            if rc == 0:
                return Path(found)
    return None


def build_dashboard(
    web_dir: Path,
    *,
    on_output: Callable[[str], None] | None = None,
    confirm: Callable[[str], bool] | None = None,
    idle_timeout: float = 180.0,
) -> BuildOutcome:
    """Install deps and build the SPA. Never raises.

    ``confirm``: asked before downloading a Node runtime. None means "cannot
    ask" (background service, non-interactive) — then a missing Node is simply
    reported, never silently downloaded.

    The build targets a STAGING directory under ``web_dir`` (named ``dist.staging``)
    and only swaps it into ``web_dir/dist`` after the staging result passes
    validation. Vite's ``emptyOutDir: true`` clears whatever was at the
    configured outDir the moment the build starts, so writing to ``dist``
    directly would destroy the previous bundle even on a failed build — the
    staging indirection keeps the previous bundle available across any failure.

    Two outcomes are reported independently: ``build_succeeded`` (the build
    command reported success and produced a usable artifact) and
    ``artifact_usable`` (a verified bundle is now installed under
    ``web_dir/dist``). Callers must branch on ``artifact_usable`` to know
    whether to serve the Dashboard; ``build_succeeded`` alone is for "did
    the build command succeed" UX.
    """
    staging = web_dir / "dist.staging"
    dist = web_dir / "dist"
    had_dist = (dist / "index.html").is_file()

    def fail(reason: BuildReason, detail: str) -> BuildOutcome:
        """Build command failed OR staging result was invalid. The previous
        ``web_dir/dist`` is untouched in both cases (vite wrote to staging,
        not dist). artifact_usable stays whatever it was — there is no point
        lying about the bundle state.
        """
        return BuildOutcome(
            build_succeeded=False,
            artifact_usable=had_dist,
            reason=reason, detail=detail,
            had_previous_bundle=had_dist,
        )

    node = _find_usable_node()
    if node is None:
        if confirm is None:
            return BuildOutcome(
                build_succeeded=False, artifact_usable=had_dist,
                reason=BuildReason.NODE_MISSING,
                detail=f"Node.js >= {MIN_NODE_MAJOR} not found",
                had_previous_bundle=had_dist,
            )
        if not confirm(
            f"构建完整 Dashboard 需要 Node.js {MIN_NODE_MAJOR}+，本机未找到可用版本。"
            "是否下载并安装到 ~/.echo-agent/node（约 30MB）？"
        ):
            return BuildOutcome(
                build_succeeded=False, artifact_usable=had_dist,
                reason=BuildReason.NODE_DECLINED,
                detail="user declined the Node download",
                had_previous_bundle=had_dist,
            )
        node = _download_node(on_output)
        if node is None:
            return BuildOutcome(
                build_succeeded=False, artifact_usable=had_dist,
                reason=BuildReason.NODE_TOO_OLD,
                detail="Node installation did not succeed",
                had_previous_bundle=had_dist,
            )

    pnpm = _ensure_pnpm(node, on_output)
    if pnpm is None:
        return BuildOutcome(
            build_succeeded=False, artifact_usable=had_dist,
            reason=BuildReason.PNPM_UNAVAILABLE,
            detail="could not obtain a working pnpm",
            had_previous_bundle=had_dist,
        )

    env = {**os.environ,
           "PATH": f"{node.parent}{os.pathsep}{os.environ.get('PATH', '')}"}

    # Fresh staging for every run. Vite's emptyOutDir clears the directory
    # the moment it opens for write — keeping a stale staging around would
    # risk build A seeing build B's leftovers if A's pnpm install wrote
    # nothing.
    if staging.exists():
        try:
            shutil.rmtree(staging)
        except OSError as e:
            return BuildOutcome(
                build_succeeded=False, artifact_usable=had_dist,
                reason=BuildReason.BUILD_FAILED,
                detail=f"failed to clear staging: {e}",
                had_previous_bundle=had_dist,
            )

    # Point Vite at the staging directory for THIS build only. The web/vite
    # default (``dist``) is left alone in source — the gateway is the one
    # swapping artifacts, and overwriting that default would make a local
    # ``pnpm build`` outside the gateway go into staging too.
    build_env = {**env, "ECHO_DASHBOARD_OUT_DIR": str(staging)}

    rc, output = _run_streamed(
        [str(pnpm), "install", "--frozen-lockfile"], cwd=web_dir,
        on_output=on_output, idle_timeout=idle_timeout, env=build_env,
    )
    if rc != 0:
        return BuildOutcome(
            build_succeeded=False, artifact_usable=had_dist,
            reason=BuildReason.INSTALL_FAILED,
            detail=output[-2000:], had_previous_bundle=had_dist,
        )

    rc, output = _run_streamed(
        [str(pnpm), "build"], cwd=web_dir,
        on_output=on_output, idle_timeout=idle_timeout, env=build_env,
    )
    if rc != 0:
        # One retry: transient causes (antivirus scanning the Node binary, npm
        # cache not ready, boot-time I/O contention) clear on their own.
        _sleep(3)
        rc, output = _run_streamed(
            [str(pnpm), "build"], cwd=web_dir,
            on_output=on_output, idle_timeout=idle_timeout, env=build_env,
        )
    if rc != 0:
        return BuildOutcome(
            build_succeeded=False, artifact_usable=had_dist,
            reason=BuildReason.BUILD_FAILED,
            detail=output[-2000:], had_previous_bundle=had_dist,
        )

    # Validate the staging result before swapping. "pnpm build exited 0" is
    # necessary but not sufficient — Vite can complete without producing an
    # index.html on certain misconfigurations (the static-path tree is
    # non-trivial), and serving an empty dist is worse than refusing to swap.
    staging_index = staging / "index.html"
    if not staging_index.is_file():
        return BuildOutcome(
            build_succeeded=False, artifact_usable=had_dist,
            reason=BuildReason.BUILD_FAILED,
            detail="build reported success but dist.staging/index.html is missing",
            had_previous_bundle=had_dist,
        )

    # Atomic swap. ``os.replace`` cannot move one non-empty directory on top of
    # another non-empty directory on macOS (EINVAL/[Errno 66] "Directory not
    # empty"), so a single rename is not enough. The portable two-step is:
    # rename the old dist aside (atomic), then rename staging into place
    # (atomic). At every instant, exactly one directory sits at web_dir/dist:
    # the old bundle until step 1, then the new bundle from step 2 onward. A
    # reader mid-swap never sees a half-deleted dist. If step 1 succeeds but
    # step 2 fails, the backup is rolled forward into dist so we never leave
    # the gateway without a bundle.
    backup = web_dir / "dist.swapbackup"
    # If a previous swap failed mid-flight, the backup may still be around.
    # Best-effort clear before starting; if it is the only copy we have left,
    # step 1 below will move it elsewhere anyway.
    if backup.exists():
        try:
            shutil.rmtree(backup)
        except OSError:
            # Inability to clear is not fatal — the rename below would
            # overwrite or fail loudly. Surface as a build failure rather
            # than silently corrupt the swap.
            return BuildOutcome(
                build_succeeded=False, artifact_usable=had_dist,
                reason=BuildReason.BUILD_FAILED,
                detail=f"could not clear leftover swap backup at {backup}",
                had_previous_bundle=had_dist,
            )

    if dist.exists():
        try:
            os.rename(dist, backup)
        except OSError as e:
            return BuildOutcome(
                build_succeeded=False, artifact_usable=had_dist,
                reason=BuildReason.BUILD_FAILED,
                detail=f"could not move old dist aside for swap: {e}",
                had_previous_bundle=had_dist,
            )
    try:
        os.rename(staging, dist)
    except OSError as e:
        # Step 2 failed. Roll back the old bundle so the gateway keeps
        # serving it.
        if backup.exists():
            try:
                os.rename(backup, dist)
            except OSError as rollback_err:
                return BuildOutcome(
                    build_succeeded=False, artifact_usable=False,
                    reason=BuildReason.BUILD_FAILED,
                    detail=(
                        f"swap failed and rollback failed: swap={e}, "
                        f"rollback={rollback_err}; old dist at {backup}"
                    ),
                    had_previous_bundle=had_dist,
                )
        return BuildOutcome(
            build_succeeded=False, artifact_usable=had_dist,
            reason=BuildReason.BUILD_FAILED,
            detail=f"build succeeded but atomic swap into web/dist failed: {e}",
            had_previous_bundle=had_dist,
        )
    # Swap complete: old bundle is now at ``backup``, new bundle at ``dist``.
    # Remove the backup — failure is cosmetic (a stale backup that gets
    # cleared at the next successful swap), not a functional issue.
    try:
        shutil.rmtree(backup)
    except OSError:
        pass

    return BuildOutcome(
        build_succeeded=True,
        artifact_usable=True,
        reason=BuildReason.OK,
        had_previous_bundle=had_dist,
    )


def _is_tty() -> bool:
    try:
        return bool(sys.stdout.isatty())
    except (AttributeError, ValueError):
        return False


def _in_service_process() -> bool:
    """True when this process was started by launchd/systemd.

    A supervised gateway must never stop to build a frontend: the unit would sit
    in "activating" for minutes, the port would stay closed, and a restart-on-
    failure policy could loop on it.
    """
    return os.environ.get(GATEWAY_ENV_FLAG) == "1"


def maybe_build_dashboard(*, interactive: bool | None = None) -> BuildOutcome | None:
    """Build the SPA if this process is allowed to, else return None.

    None means "not attempted, and that is correct" — a wheel install, a fresh
    artifact, a background service, or a non-interactive shell. Callers treat it
    as "carry on and serve whatever exists".

    ``interactive`` forces the decision (used by `echo-agent dashboard build`,
    where the user asked explicitly and the TTY check is beside the point).
    """
    if interactive is None:
        if _in_service_process() or not _is_tty():
            return None
        interactive = True

    web_dir = find_web_dir()
    if web_dir is None:
        return None
    if not dashboard_build_needed(web_dir):
        return None

    def confirm(message: str) -> bool:
        try:
            answer = input(f"{message} [Y/n] ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return False
        return answer in ("", "y", "yes")

    return build_dashboard(
        web_dir,
        on_output=lambda line: print(f"    {line}", flush=True),
        confirm=confirm if interactive else None,
    )


def describe_outcome(outcome: BuildOutcome) -> str:
    """A diagnosis the user can act on, per failure cause.

    install.sh printed one message for every cause, and it was wrong for the
    most common one: it told users on Node 20 to install pnpm@10, when the real
    fix was upgrading Node.
    """
    if outcome.reason is BuildReason.OK:
        return "完整 Dashboard 已构建。"

    # 按失败原因给可操作建议。这些提示原先写在函数末尾,但上面两条 return 是无条件的,
    # 整张 hints 表从来没被执行过 —— Node 缺失/版本过低/pnpm 不可用/依赖安装失败这些
    # 最常见的情况,用户只会看到笼统的"构建失败",拿不到该怎么修。
    hints = {
        BuildReason.NODE_MISSING: (
            f"未找到 Node.js {MIN_NODE_MAJOR}+，跳过 Dashboard 构建。"
            "\n在交互式终端运行 echo-agent dashboard build 可下载并构建。"
        ),
        BuildReason.NODE_TOO_OLD: (
            f"Node.js 版本低于 {MIN_NODE_MAJOR}，无法构建 Dashboard。"
            f"\n请升级 Node.js 到 {MIN_NODE_MAJOR}+ 后重试：echo-agent dashboard build"
        ),
        BuildReason.NODE_DECLINED: (
            "已跳过 Node.js 下载，当前使用内置简化页。"
            "\n需要完整 Dashboard 时运行：echo-agent dashboard build"
        ),
        BuildReason.PNPM_UNAVAILABLE: (
            "无法获得可用的 pnpm，跳过 Dashboard 构建。"
            f"\n可手动安装：npm i -g pnpm@{PNPM_FALLBACK_MAJOR}，然后运行 echo-agent dashboard build"
        ),
        BuildReason.INSTALL_FAILED: (
            "前端依赖安装失败（网络问题，或 pnpm-lock.yaml 与 package.json 不同步）。"
            "\n重试：echo-agent dashboard build"
        ),
        BuildReason.BUILD_FAILED: "前端构建失败。\n重试：echo-agent dashboard build",
    }
    base = hints.get(outcome.reason)
    if outcome.artifact_usable:
        # 构建失败但上一份产物还在。两件事都要说清:这次构建没成,gateway 继续用旧产物。
        # 早先的实现把这种情况报成成功,与用户刚刚看到的失败自相矛盾。
        head = "构建失败，继续使用上一次的 Dashboard 产物（可能已过期）。"
    else:
        head = "构建失败，未产生可服务的 Dashboard 产物。"
    parts = [head]
    if base:
        parts.append(base)
    if outcome.detail:
        parts.append(f"原因：{outcome.detail[:400]}")
    return "\n".join(parts)
