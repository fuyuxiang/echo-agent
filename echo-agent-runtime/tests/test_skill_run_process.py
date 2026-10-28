"""Tests for skill_run's subprocess lifecycle and environment.

Two defects here were deterministic, not edge cases:

* The timeout parameter accepted up to 600s while the tool's own
  ``timeout_seconds`` was 120, so the registry's outer ``wait_for`` cancelled any
  skill running longer than two minutes — and the ``CancelledError`` path never
  killed the child, so it kept running detached.
* ``env={}`` meant no PATH, no HOME and no credentials (see test_skill_env).
"""

from __future__ import annotations

import asyncio
import pathlib
import sys

import pytest

from echo_agent.agent.tools.skill_run import SkillRunTool, _MAX_SCRIPT_TIMEOUT
from echo_agent.skills.store import SkillStore


def _make_skill(user_dir, name="runner", script_body="print('ok')\n", frontmatter_extra=""):
    d = user_dir / name
    (d / "scripts").mkdir(parents=True)
    (d / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: d\n{frontmatter_extra}---\n\nbody\n",
        encoding="utf-8",
    )
    (d / "scripts" / "run.py").write_text(script_body, encoding="utf-8")
    return d


@pytest.fixture
def user_dir(tmp_path):
    d = tmp_path / "user"
    d.mkdir()
    return d


class TestTimeoutLayering:
    def test_tool_timeout_exceeds_max_script_timeout(self):
        """The registry wraps execute() in wait_for(timeout_seconds); if that sits
        below what the timeout parameter allows, long skills are killed from the
        outside and reported as failures while still running."""
        assert SkillRunTool.timeout_seconds > _MAX_SCRIPT_TIMEOUT

    def test_parameter_is_clamped_to_documented_max(self, user_dir):
        assert f"max {_MAX_SCRIPT_TIMEOUT}" in SkillRunTool.parameters[
            "properties"]["timeout"]["description"]


class TestProcessReaping:
    @pytest.mark.asyncio
    async def test_timeout_kills_the_child(self, user_dir):
        _make_skill(user_dir, script_body="import time\ntime.sleep(30)\n")
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute(
            {"name": "runner", "script": "scripts/run.py", "timeout": 1}, None,
        )
        assert result.success is False
        assert result.error_kind == "timeout"

    @pytest.mark.asyncio
    async def test_outer_cancel_does_not_orphan_the_child(self, user_dir, tmp_path):
        """The regression that mattered: an outer cancel used to leave the
        subprocess running, detached from any supervision.

        The child writes a marker file in a loop; after cancelling we confirm it
        stops growing, i.e. the process is actually gone.
        """
        marker = tmp_path / "alive.txt"
        body = (
            "import time\n"
            f"p = r'{marker}'\n"
            "for i in range(200):\n"
            "    open(p, 'a').write('x')\n"
            "    time.sleep(0.05)\n"
        )
        _make_skill(user_dir, script_body=body)
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        task = asyncio.create_task(
            tool.execute({"name": "runner", "script": "scripts/run.py", "timeout": 60}, None)
        )
        # Let the child start and write something.
        for _ in range(40):
            await asyncio.sleep(0.05)
            if marker.exists() and marker.stat().st_size > 0:
                break
        assert marker.exists(), "child never started"

        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

        # Give any surviving process time to keep writing.
        size_after_cancel = marker.stat().st_size
        await asyncio.sleep(0.6)
        assert marker.stat().st_size == size_after_cancel, (
            "subprocess kept running after the tool call was cancelled"
        )


class TestProcessGroupReaping:
    """Skill scripts spawn their own children (ffmpeg, pip, another script).

    Killing only the direct child left those running detached — the previous
    hand-rolled _terminate did exactly that. skill_run now goes through
    proc_lifecycle.spawn_exec/communicate_owned like every other one-shot exec
    tool, so the
    whole process group is swept.
    """

    def _spawner_body(self, marker) -> str:
        return (
            "import subprocess, sys, time\n"
            "subprocess.Popen([sys.executable, '-c',\n"
            f"  \"import time\\nf=open(r'{marker}','a')\\n\"\n"
            "  \"\\nfor i in range(300):\\n f.write('x'); f.flush(); time.sleep(0.05)\"])\n"
            "time.sleep(30)\n"
        )

    async def _assert_grandchild_stopped(self, marker):
        size_at_stop = marker.stat().st_size if marker.exists() else 0
        await asyncio.sleep(0.9)
        assert (marker.stat().st_size if marker.exists() else 0) == size_at_stop, (
            "grandchild process kept running after the tool call ended"
        )

    async def _wait_for_grandchild(self, marker):
        for _ in range(80):
            await asyncio.sleep(0.05)
            if marker.exists() and marker.stat().st_size > 0:
                return
        raise AssertionError("grandchild never started; test cannot prove anything")

    @pytest.mark.asyncio
    async def test_cancel_reaps_grandchildren(self, user_dir, tmp_path):
        marker = tmp_path / "gc_cancel.txt"
        _make_skill(user_dir, script_body=self._spawner_body(marker))
        tool = SkillRunTool(store=SkillStore(user_dir=user_dir))

        task = asyncio.create_task(
            tool.execute({"name": "runner", "script": "scripts/run.py", "timeout": 60}, None)
        )
        await self._wait_for_grandchild(marker)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await self._assert_grandchild_stopped(marker)

    @pytest.mark.asyncio
    async def test_timeout_reaps_grandchildren(self, user_dir, tmp_path):
        marker = tmp_path / "gc_timeout.txt"
        _make_skill(user_dir, script_body=self._spawner_body(marker))
        tool = SkillRunTool(store=SkillStore(user_dir=user_dir))

        result = await tool.execute(
            {"name": "runner", "script": "scripts/run.py", "timeout": 1}, None,
        )
        assert result.error_kind == "timeout"
        await self._assert_grandchild_stopped(marker)

    def test_uses_shared_process_lifecycle(self):
        """Pin the consolidation itself: a future edit reintroducing a bare
        create_subprocess_exec here would silently lose group reaping.

        Checks the parsed AST rather than the raw text, so prose in comments
        mentioning the old API does not make this pass or fail spuriously.
        """
        import ast

        tree = ast.parse(pathlib.Path("echo_agent/agent/tools/skill_run.py").read_text())
        called = {
            ast.unparse(node.func)
            for node in ast.walk(tree)
            if isinstance(node, ast.Call)
        }
        assert "spawn_exec" in called, "skill_run must spawn via proc_lifecycle"
        assert "communicate_owned" in called, (
            "skill_run must communicate and reap via proc_lifecycle"
        )
        assert not any("create_subprocess" in c for c in called), (
            "skill_run must not spawn subprocesses directly; use proc_lifecycle"
        )


class TestDisabledSkill:
    @pytest.mark.asyncio
    async def test_disabled_skill_cannot_run(self, user_dir, tmp_path):
        """The evolution gate's persist_disable() has to actually stop execution."""
        marker = tmp_path / "ran.txt"
        _make_skill(user_dir, script_body=f"open(r'{marker}', 'w').write('RAN')\n")
        store = SkillStore(user_dir=user_dir, disabled=["runner"])
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.py"}, None)
        assert result.success is False
        assert "disabled" in result.error
        assert not marker.exists(), "disabled skill executed anyway"

    @pytest.mark.asyncio
    async def test_missing_skill_says_not_found(self, user_dir):
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)
        result = await tool.execute({"name": "ghost", "script": "scripts/run.py"}, None)
        assert result.success is False
        assert "not found" in result.error


class TestExecutionEnvironment:
    @pytest.mark.asyncio
    async def test_script_receives_usable_path(self, user_dir):
        """``requires.bins`` was decoration under env={}: no PATH at all."""
        body = (
            "import os, shutil, sys\n"
            "print('PATH_SET' if os.environ.get('PATH') else 'NO_PATH')\n"
            "print('PY_FOUND' if shutil.which('python3') else 'NO_PY')\n"
        )
        _make_skill(user_dir, script_body=body)
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.py"}, None)
        assert result.success is True, result.error
        assert "PATH_SET" in result.output
        assert "PY_FOUND" in result.output

    @pytest.mark.asyncio
    async def test_declared_credential_reaches_the_script(self, user_dir, monkeypatch):
        """image-gen exited immediately before this: it reads OPENAI_API_KEY."""
        monkeypatch.setenv("DEMO_SKILL_TOKEN", "tok-123")
        _make_skill(
            user_dir,
            script_body="import os\nprint(os.environ.get('DEMO_SKILL_TOKEN', 'MISSING'))\n",
            frontmatter_extra="metadata:\n  echo:\n    requires:\n      env:\n        - DEMO_SKILL_TOKEN\n",
        )
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.py"}, None)
        assert result.success is True, result.error
        assert "tok-123" in result.output

    @pytest.mark.asyncio
    async def test_undeclared_secret_is_not_visible(self, user_dir, monkeypatch):
        monkeypatch.setenv("UNDECLARED_SECRET", "leak")
        _make_skill(
            user_dir,
            script_body="import os\nprint(os.environ.get('UNDECLARED_SECRET', 'ABSENT'))\n",
        )
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.py"}, None)
        assert result.success is True, result.error
        assert "ABSENT" in result.output
        assert "leak" not in result.output

    @pytest.mark.asyncio
    async def test_cwd_is_pinned_to_skill_root(self, user_dir):
        _make_skill(user_dir, script_body="import os\nprint(os.getcwd())\n")
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.py"}, None)
        assert result.success is True
        assert str((user_dir / "runner").resolve()) in result.output

    @pytest.mark.asyncio
    async def test_interpreter_is_the_agents_own(self, user_dir):
        _make_skill(user_dir, script_body="import sys\nprint(sys.executable)\n")
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.py"}, None)
        assert result.success is True
        assert sys.executable in result.output


class TestScriptPathValidation:
    @pytest.mark.asyncio
    async def test_escape_outside_skill_root_refused(self, user_dir, tmp_path):
        _make_skill(user_dir)
        outside = tmp_path / "evil.py"
        outside.write_text("print('pwned')\n", encoding="utf-8")
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute(
            {"name": "runner", "script": "../../evil.py"}, None,
        )
        assert result.success is False
        assert "outside" in result.error

    @pytest.mark.asyncio
    async def test_non_python_file_refused(self, user_dir):
        d = _make_skill(user_dir)
        (d / "scripts" / "run.sh").write_text("echo hi\n", encoding="utf-8")
        store = SkillStore(user_dir=user_dir)
        tool = SkillRunTool(store=store)

        result = await tool.execute({"name": "runner", "script": "scripts/run.sh"}, None)
        assert result.success is False
        assert "not a .py file" in result.error
