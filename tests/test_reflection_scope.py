from __future__ import annotations

import inspect

from echo_agent.memory import reflection as refl


def test_run_accepts_memory_scope():
    sig = inspect.signature(refl.ReflectionEngine.run)
    assert "memory_scope" in sig.parameters


def test_prefix_groups_filters_by_scope():
    src = inspect.getsource(refl.ReflectionEngine._prefix_groups)
    # 取数按 scope 收窄,不再无参全库 list_all()
    assert "session_key" in src


def test_ask_distill_uses_memory_scope_for_source_session():
    src = inspect.getsource(refl.ReflectionEngine._ask_distill)
    # 产物 source_session 用传入 memory_scope(回退 sample),不再无条件继承 entries[0]
    assert "memory_scope" in src
