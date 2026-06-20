# tests/test_document_extract.py
from pathlib import Path

import pytest

from echo_agent.agent.media.document_extract import ExtractResult, extract


def test_extract_plain_text(tmp_path: Path):
    f = tmp_path / "note.txt"
    f.write_text("hello world\nsecond line", encoding="utf-8")
    res = extract(f)
    assert isinstance(res, ExtractResult)
    assert "hello world" in res.text
    assert res.meta["format"] == "txt"
    assert res.truncated is False


def test_extract_unsupported_returns_empty(tmp_path: Path):
    f = tmp_path / "thing.bin"
    f.write_bytes(b"\x00\x01\x02")
    res = extract(f)
    assert res.text == ""
    assert res.meta["format"] == "unsupported"


def test_extract_missing_file_is_safe(tmp_path: Path):
    res = extract(tmp_path / "nope.txt")
    assert res.text == ""
    assert "error" in res.meta


def test_max_chars_truncates(tmp_path: Path):
    f = tmp_path / "big.txt"
    f.write_text("x" * 5000, encoding="utf-8")
    res = extract(f, max_chars=100)
    assert len(res.text) == 100
    assert res.truncated is True


def test_extract_pdf(tmp_path: Path):
    fitz = pytest.importorskip("pymupdf")
    f = tmp_path / "doc.pdf"
    doc = fitz.open()
    page = doc.new_page()
    page.insert_text((72, 72), "PDF page one content")
    doc.save(str(f))
    doc.close()
    res = extract(f)
    assert "PDF page one content" in res.text
    assert res.meta["format"] == "pdf"
    assert res.unit_count == 1


def test_extract_pdf_specific_page(tmp_path: Path):
    fitz = pytest.importorskip("pymupdf")
    f = tmp_path / "two.pdf"
    doc = fitz.open()
    doc.new_page().insert_text((72, 72), "PAGE_ALPHA")
    doc.new_page().insert_text((72, 72), "PAGE_BETA")
    doc.save(str(f))
    doc.close()
    res = extract(f, unit=2)
    assert "PAGE_BETA" in res.text
    assert "PAGE_ALPHA" not in res.text
