from echo_agent.memory.store import MemoryStore


def test_long_term_injection_capped(tmp_path):
    s = MemoryStore(
        memory_dir=tmp_path / "mem",
        long_term_snapshot_char_limit=100,
    )
    # write_long_term(scope, content): 写入远超上限的长期记忆分片
    s.write_long_term("", "x" * 5000)
    snap, _ids = s.get_snapshot_with_ids(session_key="")
    lt_seg = (
        snap.split("## Long-term Memory", 1)[-1]
        if "## Long-term Memory" in snap
        else ""
    )
    assert len(lt_seg) <= 200  # 100 上限 + 标题/截断标记裕量


def test_long_term_short_content_not_truncated(tmp_path):
    s = MemoryStore(
        memory_dir=tmp_path / "mem",
        long_term_snapshot_char_limit=100,
    )
    s.write_long_term("", "short note")
    snap, _ids = s.get_snapshot_with_ids(session_key="")
    assert "short note" in snap
    assert "…(truncated)" not in snap
