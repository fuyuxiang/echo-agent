import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path):
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")


def test_append_version_preserves_old_and_bumps(tmp_path):
    s = _store(tmp_path)
    old = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                            source="user_stated", source_session="x"))
    new = MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                      source="user_stated", source_session="x")
    result = s.append_version(old.id, new)
    assert result.version == old.version + 1        # 新版本 version+1
    assert s.get(old.id) is not None                # 旧版本保留
    assert s.get(old.id).superseded_by == result.id # 旧指向新
    assert not s.get(result.id).is_superseded        # 新是 active


def test_add_same_key_higher_or_equal_appends(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                      source="user_stated", source_session="x"))
    r = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                          source="user_stated", source_session="x"))
    assert r.content == "上海" and r.version == 2       # 走 append 非覆盖
    all_home = [e for e in s._entries.values() if e.key == "home"]
    assert len(all_home) == 2                            # 旧版本保留(未被覆盖)
    assert any(e.content == "北京" and e.is_superseded for e in all_home)


def test_add_same_key_lower_priority_keeps_old_no_overwrite(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                      source="user_stated", source_session="x"))
    r = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                          source="model_inferred", source_session="x"))
    assert r.content == "上海"                           # 低优先级不覆盖,保留旧
    live = [e for e in s._entries.values() if e.key == "home" and not e.is_superseded]
    assert len(live) == 1 and live[0].content == "上海"  # 无新 active 版本


def test_conflict_ignores_superseded(tmp_path):
    # 确定性 RED 构造:只让 key 下残留一个 superseded 版本(active 版本已删)。
    # 若 _find_conflict 不过滤 superseded,会把 superseded 旧版本当冲突基准,
    # 把新写当"改口"→ version 递增到 2;正确行为是忽略 superseded、当作全新
    # 首版 → version 1。只保留单一 superseded 候选,消除 set 迭代顺序的偶然性:
    # 保留 active 兄弟时,有缺陷的实现可能凑巧命中 active 而假绿(跨种子 flaky)。
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                      source="user_stated", source_session="x"))
    v2 = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                           source="user_stated", source_session="x"))  # 北京→superseded
    s.delete(v2.id)  # 删除 active 上海,key=home 下只余 superseded 北京
    # 改口广州:superseded 不应充当冲突基准,应作为全新首版落地
    r = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="广州",
                          source="user_stated", source_session="x"))
    live = [e for e in s._entries.values() if e.key == "home" and not e.is_superseded]
    assert r.version == 1                                # 未以 superseded 为基准递增
    assert len(live) == 1 and live[0].content == "广州"  # 广州为唯一 active


def test_capacity_counts_only_active(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", max_user=2)
    s.add(MemoryEntry(type=MemoryType.USER, key="a", content="v1", source="user_stated", source_session="x"))
    s.add(MemoryEntry(type=MemoryType.USER, key="a", content="v2", source="user_stated", source_session="x"))  # a: 1 active + 1 superseded
    s.add(MemoryEntry(type=MemoryType.USER, key="b", content="w", source="user_stated", source_session="x"))
    # active 计数为 2(a-v2, b),未超 max_user=2,b 不应触发淘汰。
    # 缺陷:容量计数把 superseded a-v1 也算进去→达 max→触发淘汰;而 a-v1 被
    # supersede 时 updated_at 被刷新,_evict_oldest 若不排除 superseded 会误删
    # active 的 a-v2、反留 superseded 的 a-v1。故断言 active a-v2 必须存活——
    # (原断言只查 b 存活是恒真:b 是最后写入、永不被淘汰,与 bug 无关)。
    assert any(
        e.key == "a" and not e.is_superseded and e.content == "v2"
        for e in s._entries.values()
    )                                                   # active a-v2 未被误淘汰
    # 无 active 条目被误删:a-v2 与 b 两个 active 都在
    live = [e for e in s._entries.values() if not e.is_superseded]
    assert {e.content for e in live} == {"v2", "w"}
