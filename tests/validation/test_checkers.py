# tests/validation/test_checkers.py
from pathlib import Path

import pytest

from echo_agent.validation.checkers import Diagnostic, PyCompileChecker


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
