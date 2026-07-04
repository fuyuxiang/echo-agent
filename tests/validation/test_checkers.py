# tests/validation/test_checkers.py
import shutil
from pathlib import Path

import pytest

from echo_agent.validation.checkers import (
    Diagnostic,
    PyCompileChecker,
    RuffChecker,
    default_checkers,
)


def test_pycompile_can_check_only_py(tmp_path: Path):
    c = PyCompileChecker()
    assert c.can_check(tmp_path / "a.py") is True
    assert c.can_check(tmp_path / "a.txt") is False
    assert c.can_check(tmp_path / "a.json") is False


@pytest.mark.asyncio
async def test_pycompile_clean_file_returns_no_diagnostics(tmp_path: Path):
    f = tmp_path / "ok.py"
    f.write_text("x = 1\ny = x + 2\n", encoding="utf-8")
    diags = await PyCompileChecker().check(f)
    assert diags == []


@pytest.mark.asyncio
async def test_pycompile_syntax_error_reports_diagnostic(tmp_path: Path):
    f = tmp_path / "bad.py"
    f.write_text("def broken(:\n    pass\n", encoding="utf-8")
    diags = await PyCompileChecker().check(f)
    assert len(diags) == 1
    d = diags[0]
    assert isinstance(d, Diagnostic)
    assert d.severity == "error"
    assert d.line == 1
    assert "SyntaxError" in d.message or "invalid syntax" in d.message
    assert d.code == "E999"


_HAS_RUFF = shutil.which("ruff") is not None


def test_default_checkers_includes_both():
    checkers = default_checkers()
    names = {type(c).__name__ for c in checkers}
    assert "PyCompileChecker" in names
    assert "RuffChecker" in names


def test_ruff_can_check_depends_on_availability(tmp_path: Path):
    c = RuffChecker()
    # can_check is True only when ruff is on PATH and file is .py
    assert c.can_check(tmp_path / "a.py") is _HAS_RUFF
    assert c.can_check(tmp_path / "a.txt") is False


@pytest.mark.skipif(not _HAS_RUFF, reason="ruff not installed")
@pytest.mark.asyncio
async def test_ruff_reports_undefined_name(tmp_path: Path):
    f = tmp_path / "u.py"
    f.write_text("y = undefined_name_xyz\n", encoding="utf-8")
    diags = await RuffChecker().check(f)
    codes = {d.code for d in diags}
    assert "F821" in codes
    d = next(d for d in diags if d.code == "F821")
    assert d.line == 1
    assert d.severity == "error"


@pytest.mark.skipif(not _HAS_RUFF, reason="ruff not installed")
@pytest.mark.asyncio
async def test_ruff_clean_file_returns_no_diagnostics(tmp_path: Path):
    f = tmp_path / "clean.py"
    f.write_text("x = 1\nprint(x)\n", encoding="utf-8")
    diags = await RuffChecker().check(f)
    assert diags == []
