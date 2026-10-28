"""Regression tests for install.sh's parent-shell PATH handoff.

``bash install.sh`` runs in a child process.  The installer may prepend the
command-link directory for its own setup wizard, but that export disappears
when the script exits.  Its final instructions must be based on the PATH it
inherited, not the temporarily modified child PATH.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest


_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "install.sh"

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


def _run_path_flow(tmp_path: Path, *, shell: str, path_has_link: bool = False):
    home = tmp_path / "home"
    install_dir = tmp_path / "install"
    link_dir = home / ".local" / "bin"
    entrypoint = install_dir / "venv" / "bin" / "echo-agent"
    entrypoint.parent.mkdir(parents=True)
    entrypoint.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    entrypoint.chmod(0o755)

    inherited_path = "/usr/bin:/bin"
    if path_has_link:
        inherited_path = f"{link_dir}:{inherited_path}"

    env = {
        **os.environ,
        "HOME": str(home),
        "SHELL": shell,
        "PATH": inherited_path,
        "ECHO_INSTALL_DIR": str(install_dir),
        "ECHO_COMMAND_LINK_DIR": str(link_dir),
        "ECHO_INSTALLER_SOURCE": str(_sourceable_installer(tmp_path)),
    }
    proc = subprocess.run(
        [
            "bash",
            "-c",
            'set --; source "$ECHO_INSTALLER_SOURCE"; setup_path; print_success',
        ],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout, link_dir


def test_missing_parent_path_prints_copyable_activation_command(tmp_path: Path):
    output, link_dir = _run_path_flow(tmp_path, shell="/bin/zsh")

    assert "Activate the command in this terminal:" in output
    assert f"export PATH={link_dir}:$PATH" in output
    assert "or open a new terminal" in output
    assert (link_dir / "echo-agent").is_symlink()


def test_existing_parent_path_does_not_claim_reload_is_needed(tmp_path: Path):
    output, _link_dir = _run_path_flow(
        tmp_path, shell="/bin/bash", path_has_link=True
    )

    assert "Activate the command in this terminal:" not in output


def test_fish_gets_fish_native_activation_command(tmp_path: Path):
    output, link_dir = _run_path_flow(tmp_path, shell="/usr/bin/fish")

    assert f"fish_add_path {link_dir}" in output
    assert "export PATH=" not in output
