"""SpillStore：完整文本落盘、路径清洗、独占创建、按会话分目录。

这些测试兜住 spill 的底层保证：落盘内容必须与原文字节级一致（否则
"保全完整输出"这个卖点是假的），且 suggestedName 永远不能逃出 spill 根目录。
"""
from __future__ import annotations

import pytest

from echo_agent.spill.store import SpillStore, _safe_segment


def test_save_roundtrip_preserves_content_exactly(tmp_path):
    store = SpillStore(tmp_path)
    content = "第一行\r\n中间\x00带NUL\n最后一行"
    path = store.save("sess-a", "exec", content)
    # newline="" 读回：默认的通用换行会把 \r\n 折成 \n，那样就测不出
    # 落盘是否真的逐字节保留了原始行尾。
    with open(path, encoding="utf-8", newline="") as f:
        assert f.read() == content


def test_save_groups_by_session(tmp_path):
    store = SpillStore(tmp_path)
    p1 = store.save("sess-a", "exec", "one")
    p2 = store.save("sess-a", "exec", "two")
    p3 = store.save("sess-b", "exec", "three")
    assert p1.parent == p2.parent
    assert p3.parent != p1.parent


def test_save_does_not_collide(tmp_path):
    store = SpillStore(tmp_path)
    paths = {store.save("sess-a", "exec", str(i)) for i in range(20)}
    assert len(paths) == 20


@pytest.mark.parametrize("evil", [
    "../../etc/passwd",
    "a/b",
    "..",
    "",
    "x" * 500,
    "C:\\Windows\\system32",
])
def test_safe_segment_never_escapes(tmp_path, evil):
    seg = _safe_segment(evil)
    assert "/" not in seg and "\\" not in seg
    assert seg not in ("", ".", "..")
    assert len(seg) <= 64
    store = SpillStore(tmp_path)
    path = store.save("sess-a", evil, "payload")
    assert tmp_path.resolve() in path.resolve().parents


def test_root_is_exposed(tmp_path):
    assert SpillStore(tmp_path).root == tmp_path.resolve()
