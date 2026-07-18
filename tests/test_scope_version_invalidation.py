from __future__ import annotations

import inspect


def test_loop_has_scope_versions():
    from echo_agent.agent import loop
    src = inspect.getsource(loop.AgentLoop.__init__)
    assert "_scope_versions" in src


def test_invalidate_bumps_version_not_pop():
    from echo_agent.agent import loop
    src = inspect.getsource(loop.AgentLoop._invalidate_memory_caches)
    # per-scope 分支改为 bump 版本,不再 pop 单个 session key
    assert "_scope_versions" in src


def test_retrieval_cache_entry_has_scope_version():
    from echo_agent.memory.prefetch import RetrievalCacheEntry
    import dataclasses
    names = {f.name for f in dataclasses.fields(RetrievalCacheEntry)}
    assert "scope" in names and "scope_version" in names
