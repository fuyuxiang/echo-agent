"""A standalone Runtime tree must install without a repository or parent files."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import shlex
import subprocess
import sys

import pytest


INSTALLER = Path(__file__).resolve().parents[1] / "scripts/install.sh"
pytestmark = pytest.mark.skipif(shutil.which("bash") is None, reason="requires bash")


def source_tree(path: Path) -> Path:
    (path / "echo_agent").mkdir(parents=True)
    (path / "echo_agent/app.py").write_text("")
    (path / "pyproject.toml").write_text('[project]\nname="echo-agent"\nversion="0.3.8"\n')
    return path


def run_functions(tmp_path: Path, commands: str, args: list[str], **environment: str) -> subprocess.CompletedProcess:
    source, marker, trailer = INSTALLER.read_text().rpartition("\nmain\n")
    assert marker and not trailer.strip()
    script = tmp_path / "functions.sh"
    script.write_text(source + "\n" + commands)
    env = {k: v for k, v in os.environ.items() if not k.startswith("ECHO_")}
    env.update(environment)
    return subprocess.run(["bash", str(script), *args], cwd=tmp_path, env=env, text=True, capture_output=True, timeout=30)


@pytest.mark.parametrize("monorepo", [False, True])
def test_local_archive_needs_no_git_and_resolves_separate_venv(tmp_path: Path, monorepo: bool):
    parent = tmp_path / "download with spaces"
    runtime = source_tree(parent / "echo-agent-runtime" if monorepo else parent)
    install = tmp_path / "installed env"
    result = run_functions(
        tmp_path,
        'git() { echo "Git must not run" >&2; return 71; }\n'
        'prepare_runtime_source\n'
        'printf "RUNTIME=%s\\nVENV=%s\\n" "$RUNTIME_DIR" "$INSTALL_DIR"\n',
        ["--source", str(parent), "--dir", str(install)],
    )
    assert result.returncode == 0, result.stderr + result.stdout
    assert f"RUNTIME={runtime}" in result.stdout
    assert f"VENV={install}" in result.stdout
    assert not (runtime / ".git").exists()


def test_running_inside_downloaded_runtime_automatically_uses_local_sources(tmp_path: Path):
    runtime = source_tree(tmp_path / "runtime")
    (runtime / "scripts").mkdir()
    source = INSTALLER.read_text().rpartition("\nmain\n")[0]
    script = runtime / "scripts/install.sh"
    script.write_text(source + '\nprepare_runtime_source\nprintf "SOURCE=%s\\n" "$RUNTIME_DIR"\n')
    env = {k: v for k, v in os.environ.items() if not k.startswith("ECHO_")}
    result = subprocess.run(["bash", str(script)], env=env, cwd=tmp_path, text=True, capture_output=True, timeout=30)
    assert result.returncode == 0, result.stderr + result.stdout
    assert f"SOURCE={runtime}" in result.stdout


def test_invalid_local_source_fails_before_network_or_installation(tmp_path: Path):
    result = run_functions(tmp_path, 'echo "INSTALLATION_STARTED"\n', ["--source", str(tmp_path)])
    assert result.returncode != 0
    assert "Not an Echo Agent Runtime source directory" in result.stderr
    assert "INSTALLATION_STARTED" not in result.stdout


def test_prebuilt_archive_needs_no_frontend_toolchain(tmp_path: Path):
    runtime = source_tree(tmp_path / "runtime")
    (runtime / "web/dist").mkdir(parents=True)
    (runtime / "web/package.json").write_text("{}")
    (runtime / "web/dist/index.html").write_text("<!doctype html>")
    python = runtime / "venv/bin/python"
    python.parent.mkdir(parents=True)
    python.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$@"\n')
    python.chmod(0o755)
    result = run_functions(
        tmp_path,
        'prepare_runtime_source\nprepare_dashboard\necho "DASHBOARD_READY"\n',
        ["--source", str(runtime)],
    )
    assert result.returncode == 0, result.stderr + result.stdout
    assert "DASHBOARD_READY" in result.stdout
    assert "Dashboard assets are up to date" in result.stdout


def test_source_upgrade_rebuilds_an_outdated_dashboard(tmp_path: Path):
    runtime = source_tree(tmp_path / "runtime")
    (runtime / "web/dist").mkdir(parents=True)
    (runtime / "web/package.json").write_text("{}")
    index = runtime / "web/dist/index.html"
    index.write_text("old dashboard")
    os.utime(index, (1, 1))
    python = runtime / "venv/bin/python"
    python.parent.mkdir(parents=True)
    # Run the installer's real Python freshness check, replacing only the
    # expensive frontend compiler at its boundary.
    python.write_text(
        f"#!{sys.executable}\n"
        "import sys\n"
        "from echo_agent.gateway import dashboard_build as module\n"
        "def compile_frontend(web, **kwargs):\n"
        "    (web / 'dist/index.html').write_text('rebuilt dashboard')\n"
        "    return module.BuildOutcome(True, True, module.BuildReason.OK)\n"
        "module.build_dashboard = compile_frontend\n"
        "sys.argv = sys.argv[1:]\n"
        "exec(compile(sys.stdin.read(), '<installer>', 'exec'))\n"
    )
    python.chmod(0o755)
    result = run_functions(tmp_path, 'prepare_runtime_source\nprepare_dashboard\n', ["--source", str(runtime)])
    assert result.returncode == 0, result.stderr + result.stdout
    assert index.read_text() == "rebuilt dashboard"


def test_dashboard_failure_stops_before_wizard_and_service(tmp_path: Path):
    runtime = source_tree(tmp_path / "runtime")
    commands = '\n'.join(
        f"{name}() {{ :; }}"
        for name in (
            "print_banner", "detect_os", "check_prerequisites", "prepare_home", "check_network",
            "install_uv", "check_python", "prepare_runtime_source", "setup_venv", "install_deps",
        )
    )
    commands += '\nprepare_dashboard() { return 37; }\n'
    commands += 'run_setup_wizard() { echo "WIZARD_STARTED"; }\nsetup_service() { echo "SERVICE_STARTED"; }\nmain\n'
    result = run_functions(tmp_path, commands, ["--source", str(runtime)])
    assert result.returncode == 37
    assert "WIZARD_STARTED" not in result.stdout
    assert "SERVICE_STARTED" not in result.stdout


@pytest.mark.skipif(shutil.which("git") is None, reason="requires git")
@pytest.mark.parametrize("legacy_layout", [False, True])
def test_sparse_remote_checkout_handles_current_and_historical_layouts(tmp_path: Path, legacy_layout: bool):
    repository = tmp_path / "origin"
    repository.mkdir()
    runtime = source_tree(repository if legacy_layout else repository / "echo-agent-runtime")
    (repository / "client").mkdir()
    (repository / "client/desktop.txt").write_text("desktop sources")
    for args in (
        ["init", "-q", str(repository)],
        ["-C", str(repository), "add", "."],
        ["-C", str(repository), "-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "fixture"],
    ):
        subprocess.run(["git", *args], check=True, capture_output=True)
    checkout = tmp_path / "checkout"
    subprocess.run(["git", "clone", "--sparse", str(repository), str(checkout)], check=True, capture_output=True)
    result = run_functions(
        tmp_path,
        'configure_runtime_checkout\n',
        ["--dir", str(checkout)],
    )
    assert result.returncode == 0, result.stderr + result.stdout
    relative = runtime.relative_to(repository)
    assert (checkout / relative / "echo_agent/app.py").is_file()
    assert (checkout / "client/desktop.txt").exists() == legacy_layout
