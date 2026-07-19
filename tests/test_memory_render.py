from echo_agent.memory.render import render_memory_md
from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier


def _e(key, content, **kw):
    return MemoryEntry(type=kw.pop("type", MemoryType.USER), key=key, content=content, **kw)


def test_render_groups_by_key_prefix_and_idempotent():
    entries = [
        _e("profile:city", "上海"),
        _e("profile:job", "工程师"),
        _e("pref:lang", "中文"),
    ]
    out1 = render_memory_md(entries)
    out2 = render_memory_md(list(reversed(entries)))  # 输入顺序不同
    assert out1 == out2                          # 幂等:与输入顺序无关
    assert "## profile" in out1 and "## pref" in out1
    assert "上海" in out1 and "中文" in out1


def test_render_excludes_superseded_and_archival():
    entries = [
        _e("a", "active"),
        _e("b", "old", superseded_by="x"),
        _e("c", "archived", tier=MemoryTier.ARCHIVAL),
    ]
    out = render_memory_md(entries)
    assert "active" in out and "old" not in out and "archived" not in out


def test_render_char_limit():
    entries = [_e(f"k{i}", "x" * 100) for i in range(50)]
    out = render_memory_md(entries, max_chars=200)
    assert len(out) <= 220 and "…(truncated)" in out
