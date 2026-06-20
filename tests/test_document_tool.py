# tests/test_document_tool.py
from pathlib import Path

import pytest

from echo_agent.agent.tools.document import ReadDocumentTool


@pytest.mark.asyncio
async def test_read_document_full_text(tmp_path: Path):
    f = tmp_path / "note.txt"
    f.write_text("alpha beta gamma", encoding="utf-8")
    tool = ReadDocumentTool(str(tmp_path))
    res = await tool.execute({"path": str(f)})
    assert res.success is True
    assert "alpha beta gamma" in res.output


@pytest.mark.asyncio
async def test_read_document_path_violation(tmp_path: Path):
    tool = ReadDocumentTool(str(tmp_path), restrict=True)
    res = await tool.execute({"path": "/etc/passwd"})
    assert res.success is False


@pytest.mark.asyncio
async def test_read_document_unit_for_xlsx(tmp_path: Path):
    openpyxl = pytest.importorskip("openpyxl")
    f = tmp_path / "s.xlsx"
    wb = openpyxl.Workbook()
    wb.active.title = "Alpha"
    wb["Alpha"]["A1"] = "alpha_cell"
    wb.create_sheet("Beta")["A1"] = "beta_cell"
    wb.save(str(f))
    tool = ReadDocumentTool(str(tmp_path))
    res = await tool.execute({"path": str(f), "unit": "Beta"})
    assert "beta_cell" in res.output
    assert "alpha_cell" not in res.output


def test_read_document_registered():
    from echo_agent.agent.tools import discover_tools
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    tools = discover_tools(Config(), Path("/tmp/ws_doc_test"), MessageBus())
    assert any(t.name == "read_document" for t in tools)
