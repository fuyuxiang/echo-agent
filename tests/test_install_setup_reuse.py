"""Regression tests for install.sh's existing-configuration handoff.

An upgrade must preserve a working setup without funneling the user through a
default-yes reconfiguration flow. Fresh/incomplete installs still need the
wizard, while invalid YAML must be surfaced and left untouched.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


_ROOT = Path(__file__).resolve().parents[1]
_SCRIPT = _ROOT / "scripts" / "install.sh"

pytestmark = pytest.mark.skipif(
    shutil.which("bash") is None or not _SCRIPT.exists(),
    reason="bash or scripts/install.sh unavailable",
)


def _sourceable_installer(tmp_path: Path) -> Path:
    source = _SCRIPT.read_text(encoding="utf-8")
    body, marker, trailer = source.rpartition("\nmain\n")
    assert marker and not trailer.strip(), "install.sh no longer ends with a standalone main call"
    target = tmp_path / "install-functions.sh"
    target.write_text(body + "\n", encoding="utf-8")
    return target


def _install_tree(tmp_path: Path) -> tuple[Path, Path]:
    install_dir = tmp_path / "install"
    echo_home = tmp_path / "home"
    bin_dir = install_dir / "venv" / "bin"
    bin_dir.mkdir(parents=True)
    echo_home.mkdir()
    (bin_dir / "python").symlink_to(sys.executable)
    echo_agent = bin_dir / "echo-agent"
    echo_agent.write_text(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_CALLS\"\n",
        encoding="utf-8",
    )
    echo_agent.chmod(0o755)
    return install_dir, echo_home


def _classify(tmp_path: Path, yaml_text: str | None) -> tuple[int, str]:
    install_dir, echo_home = _install_tree(tmp_path)
    if yaml_text is not None:
        (echo_home / "echo-agent.yaml").write_text(yaml_text, encoding="utf-8")
    env = {
        **os.environ,
        "ECHO_INSTALLER_SOURCE": str(_sourceable_installer(tmp_path)),
        "TEST_INSTALL_DIR": str(install_dir),
        "TEST_ECHO_HOME": str(echo_home),
        "PYTHONPATH": str(_ROOT),
    }
    proc = subprocess.run(
        [
                "bash",
                "-c",
                'set --; source "$ECHO_INSTALLER_SOURCE"; '
                'INSTALL_DIR="$TEST_INSTALL_DIR"; ECHO_HOME="$TEST_ECHO_HOME"; '
                'if output="$(existing_setup_config_state)"; then rc=0; else rc=$?; fi; '
                'printf "rc=%s\\n%s\\n" "$rc" "$output"',
        ],
        capture_output=True,
        text=True,
        timeout=30,
        cwd=_ROOT,
        env=env,
    )
    assert proc.returncode == 0, proc.stderr
    first, *rest = proc.stdout.splitlines()
    return int(first.removeprefix("rc=")), "\n".join(rest)


def _run_wizard_handoff(
    tmp_path: Path,
    *,
    config_state: int,
    force_setup: bool = False,
    run_setup: bool = True,
) -> tuple[str, list[str], list[str]]:
    install_dir, echo_home = _install_tree(tmp_path)
    calls = tmp_path / "calls"
    checks = tmp_path / "checks"
    env = {
        **os.environ,
        "ECHO_INSTALLER_SOURCE": str(_sourceable_installer(tmp_path)),
        "TEST_INSTALL_DIR": str(install_dir),
        "TEST_ECHO_HOME": str(echo_home),
        "TEST_CALLS": str(calls),
        "TEST_CHECKS": str(checks),
        "TEST_CONFIG_STATE": str(config_state),
        "TEST_FORCE_SETUP": "true" if force_setup else "false",
        "TEST_RUN_SETUP": "true" if run_setup else "false",
    }
    proc = subprocess.run(
        [
            "bash",
            "-c",
            r'''
set --
source "$ECHO_INSTALLER_SOURCE"
INSTALL_DIR="$TEST_INSTALL_DIR"
ECHO_HOME="$TEST_ECHO_HOME"
FORCE_SETUP="$TEST_FORCE_SETUP"
RUN_SETUP="$TEST_RUN_SETUP"
existing_setup_config_state() {
    printf 'checked\n' >> "$TEST_CHECKS"
    if [ "$TEST_CONFIG_STATE" -ne 1 ]; then
        printf '%s/echo-agent.yaml\n' "$ECHO_HOME"
    else
        # State 1 can still mean an existing but incomplete file.
        printf '%s/echo-agent.yaml\n' "$ECHO_HOME"
    fi
    return "$TEST_CONFIG_STATE"
}
prompt_yes_no() {
    printf 'prompt:%s\n' "$1" >> "$TEST_CALLS"
    return 0
}
service_installed_by_wizard() { return 1; }
run_setup_wizard
''',
        ],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert proc.returncode == 0, proc.stderr
    return (
        proc.stdout,
        calls.read_text(encoding="utf-8").splitlines() if calls.exists() else [],
        checks.read_text(encoding="utf-8").splitlines() if checks.exists() else [],
    )


def test_real_classifier_accepts_valid_named_provider(tmp_path: Path):
    rc, path = _classify(
        tmp_path,
        "models:\n  providers:\n    - name: openai\n",
    )
    assert rc == 0
    assert path.endswith("echo-agent.yaml")


def test_real_classifier_marks_missing_or_incomplete_config(tmp_path: Path):
    missing_rc, missing_path = _classify(tmp_path / "missing", None)
    incomplete_rc, incomplete_path = _classify(
        tmp_path / "incomplete",
        "models:\n  providers: []\n",
    )
    assert (missing_rc, missing_path) == (1, "")
    assert incomplete_rc == 1
    assert incomplete_path.endswith("echo-agent.yaml")


def test_real_classifier_rejects_invalid_config_without_rewriting(tmp_path: Path):
    invalid_yaml = "models: [unterminated\n"
    rc, path = _classify(tmp_path, invalid_yaml)
    assert rc == 2
    assert path.endswith("echo-agent.yaml")
    assert (tmp_path / "home" / "echo-agent.yaml").read_text(encoding="utf-8") == invalid_yaml


def test_existing_valid_config_skips_wizard(tmp_path: Path):
    output, calls, checks = _run_wizard_handoff(tmp_path, config_state=0)
    assert "Existing Echo Agent configuration detected" in output
    assert calls == []
    assert checks == ["checked"]


def test_incomplete_config_continues_to_setup(tmp_path: Path):
    output, calls, _ = _run_wizard_handoff(tmp_path, config_state=1)
    assert "configuration is incomplete" in output
    assert calls[0].startswith("prompt:Run Echo Agent setup now?")
    assert calls[1].startswith("setup -w ")


def test_invalid_config_is_preserved_and_setup_is_skipped(tmp_path: Path):
    output, calls, _ = _run_wizard_handoff(tmp_path, config_state=2)
    assert "configuration is invalid" in output
    assert "skipping setup to avoid overwriting it" in output
    assert calls == []


def test_reconfigure_forces_wizard_without_auto_detection(tmp_path: Path):
    output, calls, checks = _run_wizard_handoff(
        tmp_path, config_state=0, force_setup=True,
    )
    assert "Reconfiguration requested" in output
    assert calls[0].startswith("prompt:Run Echo Agent setup now?")
    assert calls[1].startswith("setup -w ")
    assert checks == []


def test_skip_setup_takes_precedence_without_detection(tmp_path: Path):
    output, calls, checks = _run_wizard_handoff(
        tmp_path, config_state=0, run_setup=False,
    )
    assert "Skipping setup wizard (--skip-setup)" in output
    assert calls == []
    assert checks == []


def test_skip_setup_and_reconfigure_are_mutually_exclusive(tmp_path: Path):
    proc = subprocess.run(
        [
            "bash",
            str(_sourceable_installer(tmp_path)),
            "--skip-setup",
            "--reconfigure",
        ],
        capture_output=True,
        text=True,
        timeout=30,
        cwd=_ROOT,
    )
    assert proc.returncode == 1
    assert "cannot be used together" in proc.stderr
