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


def test_separator_collision_distinct_shards(tmp_path):
    # telegram:bob 与 telegram_bob 净化后前缀相同,但必须落不同分片,不串记忆。
    s = _store(tmp_path)
    s.write_long_term("telegram:bob", "冒号版")
    s.write_long_term("telegram_bob", "下划线版")
    assert s.read_long_term("telegram:bob") == "冒号版"
    assert s.read_long_term("telegram_bob") == "下划线版"


def test_read_long_term_no_dual_read_fallback(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    # 造旧全局 MEMORY.md
    (tmp_path / "mem" / "MEMORY.md").write_text("旧全局跨scope内容", encoding="utf-8")
    # 未写过分片的 scope 读:不应回退读到旧全局(避免跨 scope 暴露)
    assert s.read_long_term("some_scope") == ""


def test_read_long_term_shard_still_read_after_removing_fallback(tmp_path):
    # 迁移后无回退:旧全局存在也不读,只读该 scope 分片。
    s = _store(tmp_path)
    (tmp_path / "MEMORY.md").write_text("旧全局记忆", encoding="utf-8")
    # 未写分片的 scope 读到空,不回退旧全局
    assert s.read_long_term("owner") == ""
    # 写入走新分片,读回分片内容,旧全局原样不动
    s.write_long_term("owner", "新分片记忆")
    assert s.read_long_term("owner") == "新分片记忆"
    assert (tmp_path / "MEMORY.md").read_text(encoding="utf-8") == "旧全局记忆"
