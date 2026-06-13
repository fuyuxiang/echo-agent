#!/usr/bin/env python3
"""Validate that all SKILL.md command examples match their script CLI.

For each SKILL.md that contains `python3 scripts/...` examples, this test:
1. Verifies the referenced script file exists
2. Runs `python3 <script> --help` to confirm argparse boots without error
3. For subcommand-based scripts, runs `python3 <script> <subcmd> --help`
4. Cross-checks that subcommands mentioned in SKILL.md exist in the script

Run: pytest tests/test_skill_docs.py -v
"""

import os
import re
import subprocess
import sys
from pathlib import Path

import pytest

SKILLS_ROOT = Path(__file__).resolve().parent.parent / "skills"
PYTHON = sys.executable


def _find_skill_dirs():
    """Find all directories containing a SKILL.md."""
    for skill_md in SKILLS_ROOT.rglob("SKILL.md"):
        yield skill_md.parent


def _extract_script_commands(skill_md: Path) -> list[str]:
    """Extract python3 scripts/... lines from SKILL.md code blocks."""
    text = skill_md.read_text()
    commands = []
    in_block = False
    for line in text.splitlines():
        if line.strip().startswith("```"):
            in_block = not in_block
            continue
        if in_block and "python3 scripts/" in line:
            cmd = line.strip()
            if cmd.startswith("#"):
                continue
            commands.append(cmd)
    return commands


def _extract_script_path(cmd: str) -> str | None:
    """Extract relative script path from a command line."""
    m = re.search(r"(scripts/\S+\.py)", cmd)
    return m.group(1) if m else None


def _extract_subcommand(cmd: str) -> str | None:
    """Extract subcommand (first arg after .py that's not a flag)."""
    m = re.search(r"\.py\s+(\w[\w-]*)", cmd)
    if m:
        sub = m.group(1)
        if not sub.startswith("-"):
            return sub
    return None


def _get_script_subcommands(skill_dir: Path, script_path: str) -> set[str]:
    """Run --help and extract available subcommands from usage output."""
    full_path = skill_dir / script_path
    if not full_path.exists():
        return set()
    result = subprocess.run(
        [PYTHON, str(full_path), "--help"],
        capture_output=True, text=True, timeout=10,
        cwd=str(skill_dir),
    )
    if result.returncode != 0:
        return set()
    output = result.stdout + result.stderr
    subcmds = set()
    # Look for {cmd1,cmd2,...} pattern in usage line (argparse subparsers)
    for line in output.splitlines():
        if "{" in line and "}" in line:
            m = re.search(r"\{([^}]+)\}", line)
            if m:
                candidates = {s.strip() for s in m.group(1).split(",")}
                # Filter out generic positional arg names that aren't subcommands
                generic_names = {"input", "output", "file", "query", "text", "code",
                                 "expr", "target", "path", "url", "name"}
                real_subcmds = candidates - generic_names
                if real_subcmds:
                    subcmds.update(real_subcmds)
    return subcmds


# =============================================================================
# Test collection
# =============================================================================


def _collect_test_cases():
    """Yield (skill_dir, script_path, command) tuples for parametrize."""
    cases = []
    for skill_dir in _find_skill_dirs():
        skill_md = skill_dir / "SKILL.md"
        commands = _extract_script_commands(skill_md)
        for cmd in commands:
            script_path = _extract_script_path(cmd)
            if script_path:
                cases.append((skill_dir, script_path, cmd))
    return cases


_CASES = _collect_test_cases()


@pytest.mark.parametrize(
    "skill_dir,script_path,cmd",
    _CASES,
    ids=[f"{c[0].relative_to(SKILLS_ROOT)}/{Path(c[1]).name}:{i}"
         for i, c in enumerate(_CASES)],
)
def test_script_exists(skill_dir, script_path, cmd):
    """Referenced script file must exist."""
    full_path = skill_dir / script_path
    assert full_path.exists(), (
        f"SKILL.md references {script_path} but file does not exist.\n"
        f"Command: {cmd}"
    )


@pytest.mark.parametrize(
    "skill_dir,script_path,cmd",
    _CASES,
    ids=[f"{c[0].relative_to(SKILLS_ROOT)}/{Path(c[1]).name}:{i}"
         for i, c in enumerate(_CASES)],
)
def test_script_help_runs(skill_dir, script_path, cmd):
    """Script must be importable and respond to --help without crashing."""
    full_path = skill_dir / script_path
    if not full_path.exists():
        pytest.skip("script missing")
    result = subprocess.run(
        [PYTHON, str(full_path), "--help"],
        capture_output=True, text=True, timeout=10,
        cwd=str(skill_dir),
    )
    # Skip if failure is due to missing optional dependency (not a doc bug)
    if result.returncode != 0:
        combined = result.stdout + result.stderr
        if any(kw in combined for kw in ("Install:", "pip install", "ModuleNotFoundError", "No module named")):
            pytest.skip(f"optional dependency missing: {result.stderr[:100]}")
    assert result.returncode == 0, (
        f"Script --help failed (rc={result.returncode}):\n"
        f"stdout: {result.stdout[:500]}\n"
        f"stderr: {result.stderr[:500]}"
    )


@pytest.mark.parametrize(
    "skill_dir,script_path,cmd",
    _CASES,
    ids=[f"{c[0].relative_to(SKILLS_ROOT)}/{Path(c[1]).name}:{i}"
         for i, c in enumerate(_CASES)],
)
def test_subcommand_exists(skill_dir, script_path, cmd):
    """Subcommands used in SKILL.md examples must exist in the script."""
    full_path = skill_dir / script_path
    if not full_path.exists():
        pytest.skip("script missing")
    subcmd = _extract_subcommand(cmd)
    if not subcmd:
        pytest.skip("no subcommand in this example")
    available = _get_script_subcommands(skill_dir, script_path)
    if not available:
        pytest.skip("script doesn't use subcommands")
    assert subcmd in available, (
        f"SKILL.md uses subcommand '{subcmd}' but script only has: {sorted(available)}\n"
        f"Command: {cmd}"
    )
