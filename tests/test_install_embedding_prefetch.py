"""Regression tests for install.sh's embedding-model prefetch routing.

The release tar uses fastembed's legacy GCS cache layout. Fastembed 0.8 may
still probe Hugging Face unless the verification load is explicitly offline;
on Xet-backed downloads that turned a complete local model into a misleading
``Fetching 5 files: 80%`` followed by a CAS 401. These tests execute the real
shell control flow with download/process helpers stubbed at the boundary.
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


def _run_prefetch(tmp_path: Path, *, release_ready: bool, command_rc: int = 0) -> str:
    install_dir = tmp_path / "install"
    cache_dir = tmp_path / "cache"
    python = install_dir / "venv" / "bin" / "python"
    python.parent.mkdir(parents=True)
    python.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    python.chmod(0o755)

    env = {
        **os.environ,
        "ECHO_INSTALLER_SOURCE": str(_sourceable_installer(tmp_path)),
        "TEST_INSTALL_DIR": str(install_dir),
        "TEST_CACHE_DIR": str(cache_dir),
        "TEST_RELEASE_RC": "0" if release_ready else "1",
        "TEST_COMMAND_RC": str(command_rc),
    }
    # Keep developer-machine HF settings from changing the expected defaults.
    env.pop("HF_HUB_OFFLINE", None)
    env.pop("HF_HUB_DISABLE_XET", None)

    proc = subprocess.run(
        [
            "bash",
            "-c",
            r'''
set --
source "$ECHO_INSTALLER_SOURCE"
INSTALL_DIR="$TEST_INSTALL_DIR"
EMBED_CACHE_DIR="$TEST_CACHE_DIR"
EMBED_MODEL="BAAI/bge-small-zh-v1.5"
EMBED_PREFETCH_TIMEOUT=30
fetch_embedding_model_from_release() { return "$TEST_RELEASE_RC"; }
run_with_timeout() {
    printf 'CALL offline=%s xet=%s endpoint=%s' \
        "${HF_HUB_OFFLINE-unset}" "${HF_HUB_DISABLE_XET-unset}" "${HF_ENDPOINT-unset}"
    printf ' arg=<%s>' "$@"
    printf '\nPYTHON_BEGIN\n'
    cat
    printf 'PYTHON_END\n'
    return "$TEST_COMMAND_RC"
}
prefetch_embedding_model
''',
        ],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout


def test_release_hit_is_verified_strictly_offline(tmp_path: Path):
    output = _run_prefetch(tmp_path, release_ready=True)

    assert "CALL offline=1 xet=1" in output
    assert "arg=<1>" in output
    assert 'kwargs["local_files_only"] = True' in output
    assert "Verifying release-cached embedding model offline" in output


def test_release_miss_uses_online_fallback_without_xet(tmp_path: Path):
    output = _run_prefetch(tmp_path, release_ready=False)

    assert "CALL offline=0 xet=1 endpoint=https://hf-mirror.com" in output
    assert "arg=<0>" in output
    assert "Prefetching local embedding model" in output


def test_online_retry_command_disables_xet_and_materializes_weights(tmp_path: Path):
    output = _run_prefetch(tmp_path, release_ready=False, command_rc=1)

    assert "HF_HUB_DISABLE_XET='1'" in output
    assert "next(iter(m.embed(['预热'])), None)" in output


def test_release_verification_failure_stays_offline(tmp_path: Path):
    output = _run_prefetch(tmp_path, release_ready=True, command_rc=1)

    assert "Release embedding cache is present but failed offline verification" in output
    assert "HF_HUB_OFFLINE=1" in output
    assert "local_files_only=True" in output
