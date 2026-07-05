from pathlib import Path
from types import SimpleNamespace

import pytest

from echo_agent.tools.base import ToolResult
from echo_agent.validation.checkers import Diagnostic
from echo_agent.validation.hook import (
    _extract_path,
    install_validation,
    make_post_tool_validation_hook,
)


class _Hooks:
    def __init__(self):
        self.registered = []

    def register(self, name, cb, plugin=""):
        self.registered.append((name, plugin))


def _cfg(enabled=True):
    return SimpleNamespace(validation=SimpleNamespace(
        enabled=enabled, timeout_sec=5.0, max_diagnostics=10, max_file_size_kb=512,
    ))


class _StubValidator:
    def __init__(self, diags):
        self._diags = diags

    async def validate(self, path):
        return list(self._diags)

    def format_diagnostics(self, diags, filename):
        return f"DIAG {len(diags)} {filename}"


def test_extract_path_per_tool():
    assert _extract_path("write_file", {"path": "a.py"}) == "a.py"
    assert _extract_path("edit_file", {"path": "b.py"}) == "b.py"
    assert _extract_path("patch", {"file_path": "c.py"}) == "c.py"
    assert _extract_path("write_file", {}) is None
    assert _extract_path("unknown", {"path": "x"}) is None


@pytest.mark.asyncio
async def test_hook_appends_diagnostics_on_error(tmp_path: Path):
    f = tmp_path / "bad.py"
    f.write_text("x = 1\n", encoding="utf-8")
    v = _StubValidator([Diagnostic("error", 1, 1, "E999", "boom")])
    hook = make_post_tool_validation_hook(v, tmp_path)
    result = ToolResult(success=True, output="Written 5 chars to bad.py")
    hr = await hook(result, "write_file", {"path": "bad.py"}, None)
    assert hr is not None
    assert "Written 5 chars" in hr.modified.output
    assert "DIAG 1 bad.py" in hr.modified.output


@pytest.mark.asyncio
async def test_hook_returns_none_when_clean(tmp_path: Path):
    f = tmp_path / "ok.py"
    f.write_text("x = 1\n", encoding="utf-8")
    hook = make_post_tool_validation_hook(_StubValidator([]), tmp_path)
    result = ToolResult(success=True, output="ok")
    assert await hook(result, "write_file", {"path": "ok.py"}, None) is None


@pytest.mark.asyncio
async def test_hook_skips_non_write_tool(tmp_path: Path):
    hook = make_post_tool_validation_hook(_StubValidator([Diagnostic("error", 1, 1, "E", "e")]), tmp_path)
    result = ToolResult(success=True, output="listing")
    assert await hook(result, "list_dir", {"path": "."}, None) is None


@pytest.mark.asyncio
async def test_hook_skips_failed_result(tmp_path: Path):
    hook = make_post_tool_validation_hook(_StubValidator([Diagnostic("error", 1, 1, "E", "e")]), tmp_path)
    result = ToolResult(success=False, error="disk full")
    assert await hook(result, "write_file", {"path": "x.py"}, None) is None


@pytest.mark.asyncio
async def test_hook_failopen_on_validator_exception(tmp_path: Path):
    class _Boom:
        async def validate(self, path):
            raise RuntimeError("boom")

        def format_diagnostics(self, diags, filename):
            return ""

    hook = make_post_tool_validation_hook(_Boom(), tmp_path)
    result = ToolResult(success=True, output="ok")
    assert await hook(result, "write_file", {"path": "x.py"}, None) is None


def test_install_skips_when_disabled(tmp_path: Path):
    hooks = _Hooks()
    assert install_validation(_cfg(enabled=False), tmp_path, hooks) is None
    assert hooks.registered == []


def test_install_registers_post_tool_hook(tmp_path: Path):
    hooks = _Hooks()
    v = install_validation(_cfg(enabled=True), tmp_path, hooks)
    assert v is not None
    assert hooks.registered == [("post_tool_call", "validation")]
