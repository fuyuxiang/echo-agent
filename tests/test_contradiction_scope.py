from __future__ import annotations

import inspect

from echo_agent.memory import consolidator as cons


def test_step3_narrows_candidates_by_scope():
    src = inspect.getsource(cons.MemoryConsolidator.sleep_consolidate)
    # 矛盾检测的比较集合按 memory_scope 收窄,不再无条件全库 _entries.values()
    assert "list_all(session_key=memory_scope" in src or "session_key=memory_scope" in src


def test_auto_resolve_requires_same_scope():
    src = inspect.getsource(cons.MemoryConsolidator._auto_resolve_same_key)
    # 同 key 还须同 scope 才裁决,避免跨 scope supersede
    assert "_same_scope" in src
