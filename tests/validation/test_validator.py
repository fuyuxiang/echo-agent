# tests/validation/test_validator.py
from pathlib import Path

import pytest

from echo_agent.validation.checkers import Diagnostic
from echo_agent.validation.validator import Validator


class _StubChecker:
    def __init__(self, suffix: str, diags: list[Diagnostic]):
        self._suffix = suffix
        self._diags = diags

    def can_check(self, path: Path) -> bool:
        return path.suffix == self._suffix

    async def check(self, path: Path) -> list[Diagnostic]:
        return list(self._diags)


@pytest.mark.asyncio
async def test_merges_and_dedups_across_checkers(tmp_path: Path):
    f = tmp_path / "a.py"
    f.write_text("x=1\n", encoding="utf-8")
    # compile and ruff both flag the same syntax error at 1:1 but with different
    # message/code text — a message-sensitive key would keep both.
    compile_diag = _StubChecker(".py", [Diagnostic("error", 1, 1, "E999", "SyntaxError: invalid syntax")])
    ruff_diag = _StubChecker(".py", [
        Diagnostic("error", 1, 1, "invalid-syntax", "Expected a parameter or the end of the parameter list"),
        Diagnostic("error", 5, 2, "F821", "undefined name"),
    ])
    v = Validator(checkers=[compile_diag, ruff_diag])
    diags = await v.validate(f)
    # (line,col) dedup collapses the two 1:1 diagnostics into one → 2 total
    assert len(diags) == 2
    assert diags[0].line == 1 and diags[1].line == 5  # sorted by (line,col)
    # last writer wins: ruff (later in the registry) overrides the compile floor
    assert diags[0].code == "invalid-syntax"
    assert diags[0].message == "Expected a parameter or the end of the parameter list"


@pytest.mark.asyncio
async def test_dedup_merges_distinct_errors_at_same_position(tmp_path: Path):
    # Documented trade-off: two genuinely different errors at the same (line,col)
    # collapse to the later one. Acceptable because in practice a shared position
    # means the same underlying syntax error phrased two ways.
    f = tmp_path / "a.py"
    f.write_text("x=1\n", encoding="utf-8")
    c = _StubChecker(".py", [
        Diagnostic("error", 3, 4, "E1", "first error"),
        Diagnostic("error", 3, 4, "E2", "second error"),
    ])
    diags = await Validator(checkers=[c]).validate(f)
    assert len(diags) == 1
    assert diags[0].code == "E2"


@pytest.mark.asyncio
async def test_filters_out_warnings(tmp_path: Path):
    f = tmp_path / "a.py"
    f.write_text("x=1\n", encoding="utf-8")
    c = _StubChecker(".py", [Diagnostic("warning", 1, 1, "W1", "w"), Diagnostic("error", 2, 1, "E1", "e")])
    diags = await Validator(checkers=[c]).validate(f)
    assert len(diags) == 1 and diags[0].severity == "error"


@pytest.mark.asyncio
async def test_validate_returns_full_list_untruncated(tmp_path: Path):
    f = tmp_path / "a.py"
    f.write_text("x=1\n", encoding="utf-8")
    # each has a distinct (line,col,message) so dedup keeps all 20
    many = [Diagnostic("error", i, 1, "E", f"e{i}") for i in range(1, 21)]
    diags = await Validator(checkers=[_StubChecker(".py", many)], max_diagnostics=5).validate(f)
    assert len(diags) == 20  # validate does NOT truncate; format_diagnostics does


@pytest.mark.asyncio
async def test_no_matching_checker_returns_empty(tmp_path: Path):
    f = tmp_path / "a.txt"
    f.write_text("hi\n", encoding="utf-8")
    diags = await Validator(checkers=[_StubChecker(".py", [Diagnostic("error", 1, 1, "E", "e")])]).validate(f)
    assert diags == []


@pytest.mark.asyncio
async def test_oversize_file_skipped(tmp_path: Path):
    f = tmp_path / "big.py"
    f.write_text("x = 1\n" * 100000, encoding="utf-8")  # well over 1 KB
    diags = await Validator(checkers=[_StubChecker(".py", [Diagnostic("error", 1, 1, "E", "e")])], max_file_size_kb=1).validate(f)
    assert diags == []


@pytest.mark.asyncio
async def test_checker_exception_is_failopen(tmp_path: Path):
    f = tmp_path / "a.py"
    f.write_text("x=1\n", encoding="utf-8")

    class _Boom:
        def can_check(self, path):
            return True

        async def check(self, path):
            raise RuntimeError("boom")

    diags = await Validator(checkers=[_Boom()]).validate(f)
    assert diags == []


def test_format_diagnostics_renders_block():
    v = Validator(checkers=[])
    diags = [Diagnostic("error", 12, 5, "F821", "undefined name 'bar'"),
             Diagnostic("error", 20, 1, "E999", "SyntaxError: invalid syntax")]
    text = v.format_diagnostics(diags, "foo.py")
    assert "写后校验发现 2 个错误 (foo.py)" in text
    assert "L12:5 undefined name 'bar' [F821]" in text
    assert "L20:1 SyntaxError: invalid syntax [E999]" in text


def test_format_diagnostics_truncates_and_notes_remainder():
    v = Validator(checkers=[], max_diagnostics=3)
    diags = [Diagnostic("error", i, 1, "E", f"e{i}") for i in range(1, 9)]  # 8 errors
    text = v.format_diagnostics(diags, "foo.py")
    assert "写后校验发现 8 个错误 (foo.py)" in text
    assert text.count("\n  L") == 3  # only 3 rendered
    assert "还有 5 个错误未列出" in text
