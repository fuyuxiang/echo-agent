from __future__ import annotations

from echo_agent.memory.store import MemoryStore


def _store(tmp_path):
    return MemoryStore(memory_dir=tmp_path)


def test_write_read_roundtrip_per_scope(tmp_path):
    s = _store(tmp_path)
    s.write_long_term("owner", "owner 的事实")
    s.write_long_term("telegram:bob", "bob 的事实")
    assert s.read_long_term("owner") == "owner 的事实"
    assert s.read_long_term("telegram:bob") == "bob 的事实"
    # 不同 scope 互不串
    assert "bob" not in s.read_long_term("owner")


def test_scope_filename_sanitized(tmp_path):
    s = _store(tmp_path)
    s.write_long_term("gateway:web:u1", "x")
    # 冒号被安全化,文件确实落盘且可读回
    assert s.read_long_term("gateway:web:u1") == "x"


def test_dual_read_falls_back_to_legacy_global(tmp_path):
    s = _store(tmp_path)
    # 模拟旧全局 MEMORY.md 存在、owner 分片不存在
    (tmp_path / "MEMORY.md").write_text("旧全局记忆", encoding="utf-8")
    assert s.read_long_term("owner") == "旧全局记忆"
    # 写入走新分片,不覆盖旧全局
    s.write_long_term("owner", "新分片记忆")
    assert s.read_long_term("owner") == "新分片记忆"
    assert (tmp_path / "MEMORY.md").read_text(encoding="utf-8") == "旧全局记忆"
