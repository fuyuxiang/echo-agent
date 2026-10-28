"""SkillRunTool pins the interpreter, cwd, and dependency handshake.

Three regressions this pins (reviewer P1-8):

1. ``python3`` in SKILL.md instructions used to resolve against the shell's
   PATH, not the venv the agent itself uses. The agent now passes its own
   ``sys.executable`` to the subprocess, and executor.base prepends the
   interpreter's bin/ onto PATH. Either layer is enough to keep a service
   started by launchd/systemd from falling off the venv.
2. A skill script invoked with the wrong cwd opens ``./templates/foo.json``
   and reads the wrong file. The tool pins cwd to the skill root.
3. A model that names a script outside the skill root (e.g.
   ``../../etc/passwd``) would read whatever it likes — refused explicitly.
"""

from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from echo_agent.agent.tools.skill_run import SkillRunTool
from echo_agent.skills.store import SkillStore


def _make_skill(workspace: Path, name: str = "demo", body: str = "", script_body: str = "") -> Path:
    skill_dir = workspace / "skills" / name
    (skill_dir / "scripts").mkdir(parents=True, exist_ok=True)
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {name}\n---\n\n{body}\n",
        encoding="utf-8",
    )
    (skill_dir / "scripts" / "hello.py").write_text(script_body, encoding="utf-8")
    return skill_dir


@pytest.fixture
def workspace(tmp_path: Path) -> Path:
    return tmp_path


def _store(workspace: Path) -> SkillStore:
    return SkillStore(
        user_dir=workspace / "user_skills",
        builtin_dir=workspace / "skills",
        external_dirs=[],
        disabled=set(),
    )


# ── cwd pin ─────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_cwd_is_pinned_to_skill_root(workspace):
    # Script prints the directory it ran in. cwd should be the skill root,
    # NOT the workspace or the agent's cwd.
    _make_skill(
        workspace,
        name="cwd-check",
        script_body=textwrap.dedent("""\
            import sys
            print("cwd=" + repr(__import__("os").getcwd()))
        """),
    )
    tool = SkillRunTool(_store(workspace))
    result = await tool.execute(
        {"name": "cwd-check", "script": "scripts/hello.py"},
        ctx=None,
    )
    assert result.success, result.error
    assert result.output.startswith("cwd=")
    # Skill root ends with the skill name as its last path segment; cwd
    # should land there (not in the workspace or the agent's cwd).
    import ast
    cwd = ast.literal_eval(result.output.split("=", 1)[1].strip())
    assert Path(cwd).name == "cwd-check", cwd


# ── interpreter pin ──────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_interpreter_is_sys_executable(workspace):
    # Print sys.executable — it must equal the agent's interpreter, not
    # "python3" resolved against an arbitrary PATH.
    _make_skill(
        workspace,
        name="interp-check",
        script_body=textwrap.dedent("""\
            import sys
            print("exe=" + sys.executable)
        """),
    )
    import sys as _sys
    tool = SkillRunTool(_store(workspace))
    result = await tool.execute(
        {"name": "interp-check", "script": "scripts/hello.py"},
        ctx=None,
    )
    assert result.success, result.error
    assert result.output == f"exe={_sys.executable}\n"


# ── escape refusal ──────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_script_outside_skill_root_is_refused(workspace):
    _make_skill(workspace, name="escape-test")
    tool = SkillRunTool(_store(workspace))
    # "../something.py" would let the script read anywhere on disk
    result = await tool.execute(
        {"name": "escape-test", "script": "../outside.py"},
        ctx=None,
    )
    assert not result.success
    assert "outside skill" in result.error.lower()


@pytest.mark.asyncio
async def test_absolute_script_path_is_refused(workspace):
    _make_skill(workspace, name="abs-test")
    tool = SkillRunTool(_store(workspace))
    result = await tool.execute(
        {"name": "abs-test", "script": "/etc/passwd"},
        ctx=None,
    )
    assert not result.success
    assert "outside skill" in result.error.lower()


@pytest.mark.asyncio
async def test_non_python_script_is_refused(workspace):
    _make_skill(workspace, name="ext-test")
    skill_dir = workspace / "skills" / "ext-test"
    (skill_dir / "scripts" / "shell.sh").write_text("echo hi\n", encoding="utf-8")
    tool = SkillRunTool(_store(workspace))
    result = await tool.execute(
        {"name": "ext-test", "script": "scripts/shell.sh"},
        ctx=None,
    )
    assert not result.success


# ── args and exit code surface to the tool result ──────────────────────────


@pytest.mark.asyncio
async def test_args_and_exit_code_are_returned(workspace):
    _make_skill(
        workspace,
        name="args-check",
        script_body=textwrap.dedent("""\
            import sys
            print("args=" + ",".join(sys.argv[1:]))
            if "--fail" in sys.argv:
                import sys as s
                s.exit(7)
        """),
    )
    tool = SkillRunTool(_store(workspace))
    result = await tool.execute(
        {"name": "args-check", "script": "scripts/hello.py",
         "args": ["--fail", "extra"]},
        ctx=None,
    )
    assert not result.success
    assert result.metadata["return_code"] == 7
    assert "--fail" in result.output
    assert "extra" in result.output


# ── unknown skill is refused ────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_unknown_skill_is_refused(workspace):
    tool = SkillRunTool(_store(workspace))
    result = await tool.execute(
        {"name": "no-such-skill", "script": "scripts/x.py"},
        ctx=None,
    )
    assert not result.success
    assert "not found" in result.error.lower()