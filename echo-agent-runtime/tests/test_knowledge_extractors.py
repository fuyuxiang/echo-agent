from pathlib import Path

import pytest

from echo_agent.knowledge.extractors import extract_text


def test_plain_text_read(tmp_path: Path):
    f = tmp_path / "a.md"
    f.write_text("# Title\nhello world", encoding="utf-8")
    assert "hello world" in extract_text(f)


def test_unknown_binary_returns_text_or_none(tmp_path: Path):
    f = tmp_path / "a.bin"
    f.write_bytes(b"\x00\x01\x02")
    # .bin 不在分派表 → 当纯文本直读(errors=replace),不崩
    assert extract_text(f) is not None


def test_docx_missing_lib_skips(monkeypatch, tmp_path: Path):
    import builtins
    real_import = builtins.__import__

    def fake_import(name, *a, **k):
        if name == "docx":
            raise ImportError("no docx")
        return real_import(name, *a, **k)

    monkeypatch.setattr(builtins, "__import__", fake_import)
    f = tmp_path / "a.docx"
    f.write_bytes(b"PK\x03\x04dummy")
    assert extract_text(f) is None  # 缺库跳过,不崩


def test_pdf_real_extraction(tmp_path: Path):
    fitz = pytest.importorskip("fitz")
    f = tmp_path / "doc.pdf"
    doc = fitz.open()
    page = doc.new_page()
    page.insert_text((72, 72), "alpha bravo charlie")
    doc.save(str(f))
    doc.close()
    text = extract_text(f)
    assert text is not None and "alpha bravo charlie" in text


def test_docx_real_extraction_paragraph_and_table(tmp_path: Path):
    docx = pytest.importorskip("docx")
    f = tmp_path / "doc.docx"
    document = docx.Document()
    document.add_paragraph("paragraph delta")
    table = document.add_table(rows=1, cols=2)
    table.rows[0].cells[0].text = "cellone"
    table.rows[0].cells[1].text = "celltwo"
    document.save(str(f))
    text = extract_text(f)
    assert text is not None
    # 段落与表格单元格文本都要被抽到(表格用制表符拼接)
    assert "paragraph delta" in text
    assert "cellone" in text and "celltwo" in text


def test_xlsx_real_extraction_sheet_and_cells(tmp_path: Path):
    openpyxl = pytest.importorskip("openpyxl")
    f = tmp_path / "book.xlsx"
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Sheet1"
    ws.append(["echo", "foxtrot"])
    ws.append([42, "golf"])
    wb.save(str(f))
    text = extract_text(f)
    assert text is not None
    # sheet 标题作表头(# Sheet1),单元格值(含数字)都要被抽到
    assert "Sheet1" in text
    assert "echo" in text and "foxtrot" in text
    assert "42" in text and "golf" in text


def test_pptx_real_extraction_shape_text(tmp_path: Path):
    pptx = pytest.importorskip("pptx")
    f = tmp_path / "deck.pptx"
    prs = pptx.Presentation()
    slide = prs.slides.add_slide(prs.slide_layouts[5])  # 仅标题的版式
    slide.shapes.title.text = "hotel india"
    prs.save(str(f))
    text = extract_text(f)
    assert text is not None and "hotel india" in text

