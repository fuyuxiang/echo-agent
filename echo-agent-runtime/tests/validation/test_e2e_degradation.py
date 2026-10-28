"""End-to-end: real checkers + real validator + hook, plus fail-open paths."""
import shutil
from pathlib import Path

import pytest

from echo_agent.tools import ToolResult
from echo_agent.validation.checkers import default_checkers
from echo_agent.validation.hook import make_post_tool_validation_hook
from echo_agent.validation.validator import Validator

_HAS_RUFF = shutil.which("ruff") is not None


def _real_hook(workspace: Path):
    v = Validator(checkers=default_checkers(), timeout_sec=5.0, max_diagnostics=10)
    return make_post_tool_validation_hook(v, workspace)


@pytest.mark.asyncio
async def test_e2e_syntax_error_feeds_back(tmp_path: Path):
    f = tmp_path / "bad.py"
    f.write_text("def broken(:\n    pass\n", encoding="utf-8")
    hook = _real_hook(tmp_path)
    result = ToolResult(success=True, output="Written to bad.py")
    hr = await hook(result, "write_file", {"path": "bad.py"}, None)
    assert hr is not None
    assert "写后校验发现" in hr.modified.output
    if _HAS_RUFF:
        # ruff overrides the compile floor at the shared position (line,col dedup)
        assert "invalid-syntax" in hr.modified.output
    else:
        # compile floor alone surfaces the syntax error as E999
        assert "E999" in hr.modified.output


@pytest.mark.asyncio
async def test_e2e_clean_file_no_feedback(tmp_path: Path):
    f = tmp_path / "clean.py"
    f.write_text("x = 1\nprint(x)\n", encoding="utf-8")
    hook = _real_hook(tmp_path)
    result = ToolResult(success=True, output="Written to clean.py")
    assert await hook(result, "write_file", {"path": "clean.py"}, None) is None


@pytest.mark.asyncio
async def test_e2e_patch_tool_uses_file_path_arg(tmp_path: Path):
    f = tmp_path / "p.py"
    f.write_text("def broken(:\n", encoding="utf-8")
    hook = _real_hook(tmp_path)
    result = ToolResult(success=True, output="patched")
    hr = await hook(result, "patch", {"file_path": "p.py"}, None)
    assert hr is not None
    assert "写后校验发现" in hr.modified.output


@pytest.mark.asyncio
async def test_e2e_non_py_file_skipped(tmp_path: Path):
    f = tmp_path / "notes.txt"
    f.write_text("this is not python (:\n", encoding="utf-8")
    hook = _real_hook(tmp_path)
    result = ToolResult(success=True, output="Written to notes.txt")
    # no checker matches .txt → clean → None
    assert await hook(result, "write_file", {"path": "notes.txt"}, None) is None


@pytest.mark.asyncio
async def test_e2e_timeout_is_failopen(tmp_path: Path):
    import asyncio

    class _Slow:
        def can_check(self, path):
            return path.suffix == ".py"

        async def check(self, path):
            await asyncio.sleep(1.0)
            return []

    f = tmp_path / "s.py"
    f.write_text("x = 1\n", encoding="utf-8")
    v = Validator(checkers=[_Slow()], timeout_sec=0.05)
    diags = await v.validate(f)
    assert diags == []  # timed out → empty, no raise


@pytest.mark.asyncio
async def test_e2e_non_dict_arguments_returns_none(tmp_path: Path):
    # Task 5 review gap: hook must fail-open on non-dict arguments (no raise).
    hook = _real_hook(tmp_path)
    result = ToolResult(success=True, output="ok")
    assert await hook(result, "write_file", None, None) is None
