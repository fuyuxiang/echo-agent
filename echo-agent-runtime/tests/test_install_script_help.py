"""CI guard: scripts/install.sh --help must describe what the script really does.

The help text drifted before: --no-mirror-probe was documented as a PyPI-only
switch after it had grown to gate the code-host and Node.js probes too, --repo
read as if it also chose the model download source, and six ECHO_* variables the
script honours were undocumented — so the only way to find them was reading the
script. These tests derive the expectations from install.sh itself, so adding a
flag or an env var without documenting it fails here rather than in a user's
terminal.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import pytest

_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "install.sh"

pytestmark = pytest.mark.skipif(
    shutil.which("bash") is None or not _SCRIPT.exists(),
    reason="bash or scripts/install.sh unavailable",
)


@pytest.fixture(scope="module")
def source() -> str:
    return _SCRIPT.read_text(encoding="utf-8")


@pytest.fixture(scope="module")
def help_text() -> str:
    proc = subprocess.run(
        ["bash", str(_SCRIPT), "--help"],
        capture_output=True, text=True, timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout


def test_help_lists_every_accepted_flag(source: str, help_text: str):
    # Flags come from the argument-parsing case arms, e.g. `--skip-setup)`.
    parser = source.split("while [[ $# -gt 0 ]]", 1)[1].split("\ndone", 1)[0]
    flags = set(re.findall(r"(--[a-z][a-z-]+)\)", parser))
    assert flags, "未能从 install.sh 解析出任何选项,测试本身失效"
    missing = sorted(f for f in flags if f not in help_text)
    assert not missing, f"install.sh --help 未说明这些选项: {missing}"


def test_help_documents_every_echo_env_var(source: str, help_text: str):
    # ECHO_HOME is an internal constant, not an input the user can set.
    internal = {"ECHO_HOME"}
    read = {
        name for name in re.findall(r"\$\{(ECHO_[A-Z_]+):", source)
    } - internal
    assert read, "未能从 install.sh 解析出任何 ECHO_* 环境变量"
    missing = sorted(name for name in read if name not in help_text)
    assert not missing, f"install.sh --help 未说明这些环境变量: {missing}"


def test_help_scopes_the_probe_switch_to_all_probes(help_text: str, source: str):
    """--no-mirror-probe gates three different probes, not just PyPI."""
    # Every guard that reads MIRROR_PROBE is one probe the flag disables.
    assert source.count("$MIRROR_PROBE") >= 3
    section = help_text.split("--no-mirror-probe", 1)[1].split("--repo", 1)[0]
    for expected in ("PyPI", "code host", "Node.js"):
        assert expected in section, f"--no-mirror-probe 说明缺少 {expected}"


def test_help_says_repo_does_not_move_the_model_source(help_text: str):
    section = help_text.split("--repo HOST", 1)[1].split("--skip-dashboard", 1)[0]
    assert "prefetch" in section.lower()
    assert "Gitee" in section


def test_help_mentions_how_to_skip_the_large_rerank_prefetch(help_text: str):
    assert "ECHO_SKIP_RERANK_PREFETCH" in help_text
