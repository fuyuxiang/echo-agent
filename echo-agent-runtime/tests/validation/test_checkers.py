# tests/validation/test_checkers.py
import asyncio
import shutil
from pathlib import Path

import pytest

from echo_agent.validation import checkers as checkers_mod
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
    # order matters: PyCompile is the floor, ruff (more specific) comes later so
    # last-writer-wins dedup lets it override the compile diagnostic at a shared
    # position.
    assert type(checkers[0]).__name__ == "PyCompileChecker"
    assert type(checkers[-1]).__name__ == "RuffChecker"


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


class _HangingProc:
    """Fake subprocess whose communicate() never completes, recording kill/wait."""

    def __init__(self) -> None:
        self.killed = False
        self.waited = False
        self.returncode = None

    async def communicate(self):
        await asyncio.Future()  # never resolves → outer wait_for must cancel it
        raise AssertionError("communicate should have been cancelled")  # pragma: no cover

    def terminate(self) -> None:
        self.killed = True

    def kill(self) -> None:
        self.killed = True

    async def wait(self) -> int:
        self.waited = True
        self.returncode = -15
        return -9


@pytest.mark.asyncio
async def test_ruff_check_kills_subprocess_on_cancel(tmp_path: Path, monkeypatch):
    """A cancelled communicate() (e.g. outer wait_for timeout) must reap the child."""
    f = tmp_path / "slow.py"
    f.write_text("x = 1\n", encoding="utf-8")

    fake = _HangingProc()

    async def _fake_spawn(*args, **kwargs):
        return fake

    monkeypatch.setattr(checkers_mod.asyncio, "create_subprocess_exec", _fake_spawn)

    checker = RuffChecker()
    # force can_check-independent path: pretend ruff is present
    checker._ruff = "/usr/bin/ruff"

    with pytest.raises(asyncio.TimeoutError):
        await asyncio.wait_for(checker.check(f), timeout=0.05)

    assert fake.killed is True
    assert fake.waited is True


class _StubProc:
    """Fake subprocess returning canned stdout bytes from communicate()."""

    def __init__(self, stdout: bytes) -> None:
        self._stdout = stdout
        # ``asyncio.subprocess.Process.communicate`` has set returncode by the
        # time it completes; model that lifecycle contract so the shared
        # one-shot cleanup helper can take its already-exited fast path.
        self.returncode = 0

    async def communicate(self):
        return self._stdout, b""

    def kill(self) -> None:  # pragma: no cover - not reached on clean exit
        pass

    async def wait(self) -> int:  # pragma: no cover
        return 0


async def _run_ruff_with_stdout(monkeypatch, tmp_path: Path, stdout: bytes) -> list[Diagnostic]:
    f = tmp_path / "x.py"
    f.write_text("x = 1\n", encoding="utf-8")

    async def _fake_spawn(*args, **kwargs):
        return _StubProc(stdout)

    monkeypatch.setattr(checkers_mod.asyncio, "create_subprocess_exec", _fake_spawn)
    checker = RuffChecker()
    checker._ruff = "/usr/bin/ruff"
    return await checker.check(f)


@pytest.mark.asyncio
async def test_ruff_non_list_json_is_failopen(tmp_path: Path, monkeypatch):
    # ruff normally emits a JSON array; a bare object must not crash the checker.
    diags = await _run_ruff_with_stdout(monkeypatch, tmp_path, b'{"unexpected": "shape"}')
    assert diags == []


@pytest.mark.asyncio
async def test_ruff_skips_malformed_entries_keeps_valid(tmp_path: Path, monkeypatch):
    # A list mixing junk (string, null) with one valid finding: junk is skipped,
    # the valid finding survives.
    payload = (
        b'["junk", null, {"code": "F821", "message": "undefined name",'
        b' "location": {"row": 3, "column": 7}}]'
    )
    diags = await _run_ruff_with_stdout(monkeypatch, tmp_path, payload)
    assert len(diags) == 1
    assert diags[0].code == "F821"
    assert diags[0].line == 3 and diags[0].col == 7


@pytest.mark.asyncio
async def test_ruff_entry_with_non_dict_location_defaults(tmp_path: Path, monkeypatch):
    # A finding whose location is not a dict falls back to line/col 1.
    payload = b'[{"code": "E1", "message": "m", "location": "bogus"}]'
    diags = await _run_ruff_with_stdout(monkeypatch, tmp_path, payload)
    assert len(diags) == 1
    assert diags[0].line == 1 and diags[0].col == 1
