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
import threading
import time
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Callable

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
    STALE_KEPT = "stale_kept"


@dataclass(frozen=True)
class BuildOutcome:
    ok: bool
    reason: BuildReason
    detail: str = ""
    used_stale: bool = False


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
    """
    had_dist = (web_dir / "dist" / "index.html").is_file()

    def stale(reason: BuildReason, detail: str) -> BuildOutcome:
        """Outcome for a build that ran and failed.

        A stale full Dashboard beats falling back to the playground, so an
        existing dist is reported as success. Only used once the toolchain is
        in place: a missing prerequisite is reported as itself (see below), not
        laundered into STALE_KEPT.
        """
        if had_dist:
            return BuildOutcome(ok=True, reason=BuildReason.STALE_KEPT,
                                detail=detail, used_stale=True)
        return BuildOutcome(ok=False, reason=reason, detail=detail)

    def unmet(reason: BuildReason, detail: str) -> BuildOutcome:
        """Outcome for a prerequisite that is missing before any build starts.

        Kept distinct from stale(): telling a user who just declined a Node
        download that the build succeeded would contradict the decision they
        made one line earlier, and the caller needs the specific reason to give
        the right next step (install Node vs. read the build log). used_stale
        still reports whether the old bundle is there to serve meanwhile.
        """
        return BuildOutcome(ok=False, reason=reason, detail=detail, used_stale=had_dist)

    node = _find_usable_node()
    if node is None:
        if confirm is None:
            return unmet(BuildReason.NODE_MISSING,
                         f"Node.js >= {MIN_NODE_MAJOR} not found")
        if not confirm(
            f"构建完整 Dashboard 需要 Node.js {MIN_NODE_MAJOR}+，本机未找到可用版本。"
            "是否下载并安装到 ~/.echo-agent/node（约 30MB）？"
        ):
            return unmet(BuildReason.NODE_DECLINED, "user declined the Node download")
        node = _download_node(on_output)
        if node is None:
            return unmet(BuildReason.NODE_TOO_OLD, "Node installation did not succeed")

    pnpm = _ensure_pnpm(node, on_output)
    if pnpm is None:
        return unmet(BuildReason.PNPM_UNAVAILABLE, "could not obtain a working pnpm")

    env = {**os.environ,
           "PATH": f"{node.parent}{os.pathsep}{os.environ.get('PATH', '')}"}

    rc, output = _run_streamed(
        [str(pnpm), "install", "--frozen-lockfile"], cwd=web_dir,
        on_output=on_output, idle_timeout=idle_timeout, env=env,
    )
    if rc != 0:
        return stale(BuildReason.INSTALL_FAILED, output[-2000:])

    rc, output = _run_streamed(
        [str(pnpm), "build"], cwd=web_dir,
        on_output=on_output, idle_timeout=idle_timeout, env=env,
    )
    if rc != 0:
        # One retry: transient causes (antivirus scanning the Node binary, npm
        # cache not ready, boot-time I/O contention) clear on their own.
        _sleep(3)
        rc, output = _run_streamed(
            [str(pnpm), "build"], cwd=web_dir,
            on_output=on_output, idle_timeout=idle_timeout, env=env,
        )
    if rc != 0:
        return stale(BuildReason.BUILD_FAILED, output[-2000:])

    if not (web_dir / "dist" / "index.html").is_file():
        return stale(BuildReason.BUILD_FAILED,
                     "build reported success but dist/index.html is missing")
    return BuildOutcome(ok=True, reason=BuildReason.OK)
