from echo_agent.memory.store import MemoryStore


def test_long_term_md_not_injected_into_snapshot(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    s.write_long_term("", "## legacy\n\n- **old**: 不该进 prompt 的 MD 文本")
    snap, ids = s.get_snapshot_with_ids(session_key="")
    assert "## Long-term Memory" not in snap  # MD 段不再注入
    assert "不该进 prompt" not in snap


def test_long_term_md_absent_even_when_large(tmp_path):
    # MD 内容再大也不进快照(不再依赖字符上限截断)。
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    s.write_long_term("", "x" * 5000)
    snap, _ids = s.get_snapshot_with_ids(session_key="")
    assert "## Long-term Memory" not in snap
    assert "xxxx" not in snap
