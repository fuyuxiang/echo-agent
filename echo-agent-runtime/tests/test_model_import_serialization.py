"""Embedding and reranking cold imports must share the same import lock."""

import threading
import time
from concurrent.futures import ThreadPoolExecutor
from types import ModuleType

from echo_agent.memory import model_import


def test_embedding_and_reranking_imports_do_not_overlap(monkeypatch):
    gate = threading.Barrier(2)
    active = 0
    peak = 0

    def import_module(name):
        nonlocal active, peak
        active += 1
        peak = max(peak, active)
        time.sleep(0.03)
        active -= 1
        return ModuleType(name)

    def load(name):
        gate.wait(timeout=2)
        return model_import.import_model_module(name)

    monkeypatch.setattr(model_import.importlib, "import_module", import_module)
    names = ["echo_agent._vendor.fastembed", "echo_agent._vendor.fastembed.rerank.cross_encoder"]
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(load, name) for name in names]
        assert [future.result(timeout=2).__name__ for future in futures] == names
    assert peak == 1
