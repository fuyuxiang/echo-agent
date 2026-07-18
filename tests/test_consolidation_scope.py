from __future__ import annotations

import inspect

from echo_agent.memory import consolidator as cons_mod


def test_consolidate_chunk_accepts_memory_scope():
    sig = inspect.signature(cons_mod.MemoryConsolidator.consolidate_chunk)
    assert "memory_scope" in sig.parameters


def test_consolidate_chunk_reads_and_writes_scoped_shard():
    src = inspect.getsource(cons_mod.MemoryConsolidator.consolidate_chunk)
    # 读写都带 scope,不再用无参全局读写
    assert "read_long_term(memory_scope" in src or "read_long_term(scope" in src
    assert "write_long_term(memory_scope" in src or "write_long_term(scope" in src
