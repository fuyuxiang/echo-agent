"""Keep the historical installer URL usable after moving component tooling."""

import os
from pathlib import Path
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[2]
pytestmark = pytest.mark.skipif(shutil.which("bash") is None, reason="requires bash")


def test_repository_bootstrap_delegates_to_runtime_help():
    result = subprocess.run(["bash", str(ROOT / "scripts/install.sh"), "--help"], text=True, capture_output=True, timeout=30)
    assert result.returncode == 0, result.stderr
    assert "--source PATH" in result.stdout
    assert "--skip-dashboard" in result.stdout


def test_downloaded_bootstrap_forwards_host_branch_and_arguments(tmp_path: Path):
    bootstrap = tmp_path / "download/install.sh"
    bootstrap.parent.mkdir()
    shutil.copy2(ROOT / "scripts/install.sh", bootstrap)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    curl = bin_dir / "curl"
    curl.write_text(
        '#!/bin/bash\n'
        'printf "%s\\n" "$@" > "$CURL_ARGS"\n'
        'while [ "$#" -gt 0 ]; do\n'
        '  if [ "$1" = "-o" ]; then shift; target="$1"; fi\n'
        '  shift\n'
        'done\n'
        'printf \'#!/bin/bash\\nprintf "ARG=<%%s>\\\\n" "$@"\\n\' > "$target"\n'
    )
    curl.chmod(0o755)
    env = {**os.environ, "PATH": str(bin_dir) + os.pathsep + os.defpath, "CURL_ARGS": str(tmp_path / "curl-args")}
    env.pop("ECHO_REPO_HOST", None)
    result = subprocess.run(
        ["bash", str(bootstrap), "--repo", "gitee", "--branch", "release/test", "--dir", "/path with spaces"],
        env=env, text=True, capture_output=True, timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert "https://gitee.com/fuyuxiang/echo-agent/raw/release/test/echo-agent-runtime/scripts/install.sh" in (tmp_path / "curl-args").read_text()
    assert "ARG=</path with spaces>" in result.stdout
