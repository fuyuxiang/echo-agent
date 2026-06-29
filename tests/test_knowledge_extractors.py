from pathlib import Path

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
