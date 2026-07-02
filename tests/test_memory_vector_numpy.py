"""numpy 版 VectorIndex：增删查、按行删、模型不一致检测。"""
from pathlib import Path

import pytest
import pytest_asyncio

from echo_agent.memory.vectors import VectorIndex
from echo_agent.storage.sqlite import SQLiteBackend

MODEL = "fastembed:BAAI/bge-small-zh-v1.5"


@pytest_asyncio.fixture
async def storage(tmp_path: Path) -> SQLiteBackend:
    backend = SQLiteBackend(tmp_path / "test.db")
    await backend.initialize()
    yield backend
    await backend.close()


@pytest_asyncio.fixture
async def index(storage: SQLiteBackend) -> VectorIndex:
    vi = VectorIndex(storage, dimensions=4, model_id=MODEL)
    await vi.initialize()
    return vi


@pytest.mark.asyncio
async def test_available_always_true(index: VectorIndex):
    assert index.available is True


@pytest.mark.asyncio
async def test_add_and_search(index: VectorIndex):
    vid = await index.add("mem1", [1.0, 0.0, 0.0, 0.0])
    assert vid
    await index.add("mem2", [0.0, 1.0, 0.0, 0.0])
    results = await index.search([1.0, 0.0, 0.0, 0.0], limit=2)
    assert results[0][0] == "mem1"
    assert results[0][1] == pytest.approx(1.0, abs=1e-5)


@pytest.mark.asyncio
async def test_add_stamps_model_and_dim(index: VectorIndex, storage: SQLiteBackend):
    await index.add("mem1", [1.0, 0.0, 0.0, 0.0])
    rows = await storage.load_vectors_all()
    assert rows[0]["model"] == MODEL
    assert rows[0]["dim"] == 4


@pytest.mark.asyncio
async def test_remove_deletes_row_immediately(index: VectorIndex, storage: SQLiteBackend):
    vid1 = await index.add("mem1", [1.0, 0.0, 0.0, 0.0])
    await index.add("mem2", [0.0, 1.0, 0.0, 0.0])
    await index.remove(vid1)
    assert index.count == 1
    results = await index.search([1.0, 0.0, 0.0, 0.0], limit=5)
    assert all(sid != "mem1" for sid, _ in results)
    rows = await storage.load_vectors_all()
    assert all(r["id"] != vid1 for r in rows)


@pytest.mark.asyncio
async def test_remove_keeps_search_correct_after_row_shift(index: VectorIndex):
    """删除中间行后，剩余向量与 source_id 映射不得错位。"""
    vid1 = await index.add("mem1", [1.0, 0.0, 0.0, 0.0])
    await index.add("mem2", [0.0, 1.0, 0.0, 0.0])
    await index.add("mem3", [0.0, 0.0, 1.0, 0.0])
    await index.remove(vid1)
    results = await index.search([0.0, 0.0, 1.0, 0.0], limit=1)
    assert results[0][0] == "mem3"


@pytest.mark.asyncio
async def test_dimension_mismatch_add_rejected(index: VectorIndex):
    assert await index.add("mem_bad", [1.0, 0.0]) == ""


@pytest.mark.asyncio
async def test_auto_dimension_adopts_first_vector(storage: SQLiteBackend):
    vi = VectorIndex(storage, dimensions=0, model_id=MODEL)
    await vi.initialize()
    await vi.add("mem1", [1.0, 0.0, 0.0])
    assert vi.dimensions == 3
    assert await vi.add("mem2", [1.0, 0.0]) == ""


@pytest.mark.asyncio
async def test_initialize_loads_only_matching_model(storage: SQLiteBackend):
    """model 不一致的存量向量不进矩阵，进 stale_source_ids 等待重嵌入。"""
    import numpy as np
    old = np.array([1.0, 0.0, 0.0, 0.0], dtype=np.float32).tobytes()
    await storage.store_vector("v_old", "mem_old", old, {}, model="openai:ada", dim=4)
    await storage.store_vector("v_legacy", "mem_legacy", old, {}, model="", dim=0)
    new = np.array([0.0, 1.0, 0.0, 0.0], dtype=np.float32).tobytes()
    await storage.store_vector("v_new", "mem_new", new, {}, model=MODEL, dim=4)

    vi = VectorIndex(storage, dimensions=4, model_id=MODEL)
    await vi.initialize()
    assert vi.count == 1
    assert vi.stale_source_ids == {"mem_old", "mem_legacy"}
    results = await vi.search([0.0, 1.0, 0.0, 0.0], limit=5)
    assert results[0][0] == "mem_new"


@pytest.mark.asyncio
async def test_rebuild_reloads_from_storage(index: VectorIndex, storage: SQLiteBackend):
    await index.add("mem1", [1.0, 0.0, 0.0, 0.0])
    await index.rebuild()
    assert index.count == 1
    results = await index.search([1.0, 0.0, 0.0, 0.0], limit=1)
    assert results[0][0] == "mem1"


@pytest.mark.asyncio
async def test_no_faiss_import():
    import echo_agent.memory.vectors as v
    import inspect
    assert "import faiss" not in inspect.getsource(v)
