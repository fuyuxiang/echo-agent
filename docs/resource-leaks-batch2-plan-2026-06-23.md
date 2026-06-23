# 第二批资源泄漏修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 echo-agent 长期运行下的四个资源泄漏点(过期会话不落库 / 快照缓存无界 / 容量淘汰留孤儿向量 / trace 文件无清理)。

**Architecture:** 四点均为定点止血,复用既有正确路径——2.1 对齐 `archive_session` 的 load-on-miss;2.2 把快照写入收口到 loop 已有的 `_lru_put`;2.3 把 `delete()` 已有的 `_cleanup_deleted` 接到淘汰路径;2.4 在 `flush_trace` 后按数量上限轮转。不引入新抽象。

**Tech Stack:** Python 3.11+、asyncio、dataclass、pydantic(config schema)、pytest。

## Global Constraints

- 测试统一用 `python -m pytest`(站点包遮蔽本地源,直接 `pytest` 可能跑到错误副本)。
- 源码根在 `echo_agent/`(非 `src/`)。
- commit message 不用 `feat:`/`fix:` 等约定式前缀,直接写中文改动描述(本仓库惯例);提交信息不含任何 Claude/Anthropic 署名。
- 新增 config 字段必须带 `json_schema_extra={"status":"effective","ref":<file:line>,"desc_zh":...,"desc_en":...}`(死字段治理要求,`tests/test_config_metadata_guard.py` 强制校验)。
- **不**把新字段写进 `config/default.yaml`:该文件不复述 schema 默认,避免 deep-merge 静默漂移(仓库惯例,见 commit 267e21e);config-reference 经 `python -m echo_agent config gen-docs` 从 schema 生成。
- 威胁模型 local-first / trusted-operator,四点均 A 类真 bug,直接提交 master。
- CI(`.github/workflows/ci.yml`):`ruff check .` 必须过、`python -m pytest tests/` 必须全绿、不新增依赖。

---

### Task 1: expire_session 缓存未命中先 load 再落库

**Files:**
- Modify: `echo_agent/session/manager.py:294-300`
- Test: `tests/test_resource_leaks_batch2.py`(新建)

**Interfaces:**
- Consumes: 既有 `SessionManager._load_from_storage(key) -> Session | None`(`manager.py:171`)、`SessionManager.save(session)`(`manager.py:234`)、`SessionManager.cleanup_expired() -> int`(`manager.py:326`)。
- Produces: 修正后的 `expire_session(key)` —— 缓存未命中时从存储 load 后改状态落库。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_resource_leaks_batch2.py
from __future__ import annotations

import asyncio
from datetime import datetime, timedelta

import pytest


@pytest.mark.asyncio
async def test_expire_session_loads_on_cache_miss(tmp_path):
    """SQLite 模式下,过期会话即使不在内存缓存,cleanup_expired 也应落库为 expired。"""
    from echo_agent.session.manager import SessionManager
    from echo_agent.storage.sqlite import SQLiteBackend

    backend = SQLiteBackend(tmp_path / "sessions.db")
    await backend.initialize()
    try:
        mgr = SessionManager(
            sessions_dir=tmp_path / "sessions",
            storage=backend,
            expiry_hours=1,
        )
        # 造一个 active 会话并落库,updated_at 设为 2 小时前(已过期)。
        sess = await mgr.get_or_create("telegram:c1")
        sess.updated_at = datetime.now() - timedelta(hours=2)
        await mgr.save(sess)
        # 清空内存缓存,模拟长驻进程里该 key 早已淘汰出缓存。
        mgr._cache.clear()

        count = await mgr.cleanup_expired()

        assert count == 1
        data = await backend.load_session("telegram:c1")
        assert data is not None
        assert data["status"] == "expired"
    finally:
        await backend.close()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_resource_leaks_batch2.py::test_expire_session_loads_on_cache_miss -v`
Expected: FAIL —— `assert data["status"] == "expired"` 得到 `"active"`(缓存未命中,`expire_session` 静默 no-op,未落库)。

> 注:若 `SessionManager` 的构造参数名(`sessions_dir`/`storage`/`expiry_hours`)与实际不符,以 `echo_agent/session/manager.py` 的 `__init__` 为准调整测试入参;断言逻辑不变。

- [ ] **Step 3: Fix expire_session**

把 `echo_agent/session/manager.py:294-300` 的 `expire_session` 替换为(对齐 `archive_session` 的 load-on-miss 模式):

```python
    async def expire_session(self, key: str) -> None:
        async with self._lock:
            session = self._cache.get(key)
        if session is None:
            session = await self._load_from_storage(key)
        if session is None:
            return
        session.status = "expired"
        await self.save(session)
```

> 要点:原实现把 `save` 放在 `if session:` 内、且 session 只来自 `_cache`。改为缓存未命中时回落 `_load_from_storage`,与 `archive_session`(`manager.py:302-315`)一致。注意不要在持有 `self._lock` 时 `await self.save`(save 内部自身取锁,见 archive_session 的写法——锁外 save)。

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_resource_leaks_batch2.py::test_expire_session_loads_on_cache_miss -v`
Expected: PASS。

- [ ] **Step 5: Run session regression**

Run: `python -m pytest tests/ -k "session" -q`
Expected: PASS(无回归;若有 `test_session_atomic.py`/`test_session_*.py` 一并绿)。

- [ ] **Step 6: Commit**

```bash
git add echo_agent/session/manager.py tests/test_resource_leaks_batch2.py
git commit -m "expire_session 缓存未命中时先从存储 load 再落库,修复 SQLite 模式过期会话清理失效"
```

---

### Task 2: _evict_oldest 同步清理向量索引 + SQLite 镜像

**Files:**
- Modify: `echo_agent/memory/store.py:480-489`
- Test: `tests/test_resource_leaks_batch2.py`(追加)

**Interfaces:**
- Consumes: 既有 `MemoryStore._cleanup_deleted(entry)`(`store.py:631`,best-effort,自带 running-loop 检测 + task 管理 + 异常吞咽)、`_unindex_entry`(`store.py:294`)。
- Produces: 修正后的 `_evict_oldest` —— 淘汰条目后调 `_cleanup_deleted`,与 `delete()` 路径对齐清理 FAISS + SQLite 镜像。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_resource_leaks_batch2.py 追加
@pytest.mark.asyncio
async def test_evict_oldest_cleans_vector_index(tmp_path):
    """容量淘汰应像 delete() 一样清理向量索引,不留 FAISS 孤儿向量。"""
    from unittest.mock import AsyncMock
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier

    removed: list[str] = []
    vector_index = AsyncMock()

    async def _remove(embedding_id):
        removed.append(embedding_id)

    vector_index.remove = _remove

    store = MemoryStore(memory_dir=tmp_path / "mem", max_user=1)
    store.set_vector_index(vector_index)

    # 写入两个 USER 条目(max_user=1),第二个触发对第一个的淘汰。
    e1 = MemoryEntry(type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
                     key="k1", content="first", source_session="s")
    e1.embedding_id = "emb-1"
    e2 = MemoryEntry(type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
                     key="k2", content="second", source_session="s")
    e2.embedding_id = "emb-2"

    store._evict_target = e1  # 占位,见下方说明
    # 直接调内部 add 路径触发淘汰;若公开 API 名不同,以 store.py 实际为准。
    await _store_entry(store, e1)
    await _store_entry(store, e2)

    # 让挂起的 _cleanup 异步任务跑完。
    await asyncio.sleep(0.05)
    assert "emb-1" in removed


async def _store_entry(store, entry):
    """复刻 store 的写入入口(实现时对齐 store.py 的真实公开方法名)。"""
    result = store.store(entry) if not asyncio.iscoroutinefunction(store.store) else await store.store(entry)
    return result
```

> **实现前必做**:打开 `echo_agent/memory/store.py`,确认写入条目的真实公开方法(可能是 `store()` / `add()` / `upsert()`,以及它是否 async、是否在 `_max_user` 满时调 `_evict_oldest`——参考 `store.py:533-535` 的 `if len(...) >= limit: self._evict_oldest(...)`)。据此把上面 `_store_entry` 改成真实调用,并删除 `store._evict_target = e1` 这行占位。`MemoryEntry` 的 `embedding_id` 字段名也以 `types.py` 为准。

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_resource_leaks_batch2.py::test_evict_oldest_cleans_vector_index -v`
Expected: FAIL —— `"emb-1" in removed` 为 False(`_evict_oldest` 只 `_unindex_entry`,从不调 `_vector_index.remove`)。

- [ ] **Step 3: Fix _evict_oldest**

把 `echo_agent/memory/store.py:480-489` 的 `_evict_oldest` 替换为:

```python
    def _evict_oldest(self, mem_type: MemoryType) -> None:
        """淘汰有效重要性最低的记忆条目，为新条目腾出空间。"""
        typed = sorted(
            self._typed_entries(mem_type),
            key=lambda entry: (self._forgetting.effective_importance(entry), entry.updated_at or "", entry.id),
        )
        if typed:
            evicted = self._entries.pop(typed[0].id, None)
            if evicted:
                self._unindex_entry(evicted)
                # 与 delete() 路径对齐:清理向量索引与 SQLite 镜像,
                # 否则容量淘汰会在 FAISS 留下孤儿向量、在镜像表留下死行。
                self._cleanup_deleted(evicted)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_resource_leaks_batch2.py::test_evict_oldest_cleans_vector_index -v`
Expected: PASS。

- [ ] **Step 5: Run memory regression**

Run: `python -m pytest tests/ -k "memory or store" -q`
Expected: PASS(无回归)。

- [ ] **Step 6: Commit**

```bash
git add echo_agent/memory/store.py tests/test_resource_leaks_batch2.py
git commit -m "记忆容量淘汰复用 _cleanup_deleted 清向量索引与镜像,消除 FAISS 孤儿向量"
```

---

### Task 3: _memory_snapshots 写入收口到 loop 统一 LRU

**Files:**
- Modify: `echo_agent/agent/loop.py`(新增 `put_memory_snapshot` 方法 + 构造 ContextStage 传参,`loop.py:286`)
- Modify: `echo_agent/agent/pipeline/context_stage.py:42-79,108-112`
- Test: `tests/test_resource_leaks_batch2.py`(追加)

**Interfaces:**
- Consumes: 既有 `AgentLoop._lru_put(cache, key, value)`(async,`loop.py:548`,用 `_state_lock` + `_max_cached_sessions=200` 上限)、`AgentLoop._memory_snapshots: OrderedDict`(`loop.py:254`)。
- Produces:
  - `AgentLoop.put_memory_snapshot(key: str, value: str) -> None`(async)—— 唯一的快照写入入口,内部走 `_lru_put`。
  - `ContextStage.__init__` 新增关键字参数 `put_snapshot: Callable[[str, str], Awaitable[None]] | None`(保留 `memory_snapshots` 引用仅用于只读 `in`/`get`)。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_resource_leaks_batch2.py 追加
@pytest.mark.asyncio
async def test_put_memory_snapshot_bounded_by_lru():
    """快照写入经 put_memory_snapshot -> _lru_put,字典不超 _max_cached_sessions,最旧被逐出。"""
    from collections import OrderedDict
    from echo_agent.agent.loop import AgentLoop

    loop = AgentLoop.__new__(AgentLoop)  # 绕过重 __init__
    import asyncio as _asyncio
    loop._state_lock = _asyncio.Lock()
    loop._memory_snapshots = OrderedDict()
    loop._max_cached_sessions = 3

    for i in range(5):
        await loop.put_memory_snapshot(f"s{i}", f"snap{i}")

    assert len(loop._memory_snapshots) == 3
    assert "s0" not in loop._memory_snapshots  # 最旧被逐出
    assert "s4" in loop._memory_snapshots      # 最新保留
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_resource_leaks_batch2.py::test_put_memory_snapshot_bounded_by_lru -v`
Expected: FAIL —— `AttributeError: 'AgentLoop' object has no attribute 'put_memory_snapshot'`。

- [ ] **Step 3: Add put_memory_snapshot to AgentLoop**

在 `echo_agent/agent/loop.py` 的 `_lru_put`(`loop.py:548`)之后、`_clear_memory_snapshot`(`loop.py:555`)之前新增:

```python
    async def put_memory_snapshot(self, key: str, value: str) -> None:
        """快照缓存的唯一写入入口:经统一 LRU 管控,
        消除 context_stage 直写 dict 带来的无界增长与双锁竞态。"""
        await self._lru_put(self._memory_snapshots, key, value)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_resource_leaks_batch2.py::test_put_memory_snapshot_bounded_by_lru -v`
Expected: PASS。

- [ ] **Step 5: Wire put_snapshot into ContextStage constructor**

在 `echo_agent/agent/pipeline/context_stage.py` 的 `__init__` 签名(`context_stage.py:42-62`)中,于 `memory_snapshots: OrderedDict,` 之后新增参数:

```python
        memory_snapshots: OrderedDict,
        put_snapshot: "Callable[[str, str], Awaitable[None]] | None" = None,
```

在赋值区(`context_stage.py:74` 附近)新增:

```python
        self._memory_snapshots = memory_snapshots
        self._put_snapshot = put_snapshot
```

确认文件顶部 import 含 `Callable`、`Awaitable`(若缺,在 `from typing import` 行补上;`from collections.abc import Callable, Awaitable` 亦可,以文件既有风格为准)。

- [ ] **Step 6: Route the write through the callback**

把 `echo_agent/agent/pipeline/context_stage.py:108-112` 的写入块:

```python
            if event.session_key not in self._memory_snapshots:
                self._memory_snapshots[event.session_key] = self._memory.get_snapshot(
                    session_key=event.session_key
                )
                self._memory_snapshots.move_to_end(event.session_key)
```

替换为:

```python
            if event.session_key not in self._memory_snapshots:
                snapshot = self._memory.get_snapshot(session_key=event.session_key)
                if self._put_snapshot is not None:
                    await self._put_snapshot(event.session_key, snapshot)
                # put_snapshot 为空时仅本轮使用、不缓存(不得回退到无界直写 dict)。
                self._snapshot_this_turn = snapshot
            else:
                self._snapshot_this_turn = self._memory_snapshots.get(event.session_key, "")
```

并把紧随其后的 `build_memory_context(..., snapshot=self._memory_snapshots.get(event.session_key, ""), ...)`(`context_stage.py:113-117`)的 `snapshot=` 实参改为 `snapshot=self._snapshot_this_turn`,确保 `put_snapshot` 为空(不缓存)时本轮仍拿到刚算出的快照。

> 说明:写入唯一入口是 `await self._put_snapshot(...)`(经 loop 的锁与上限);`_memory_snapshots` 在 context_stage 内只读(`in` 判断)。本轮快照值用局部 `self._snapshot_this_turn` 传递,避免依赖"写入后立即从 dict 读回"(写入是异步经 loop 锁,读回不保证可见)。

- [ ] **Step 7: Pass put_snapshot at construction**

在 `echo_agent/agent/loop.py:286` 构造 `ContextStage` 处,`memory_snapshots=self._memory_snapshots,` 之后新增一行:

```python
            memory_snapshots=self._memory_snapshots,
            put_snapshot=self.put_memory_snapshot,
```

- [ ] **Step 8: Run pipeline + loop regression**

Run: `python -m pytest tests/ -k "context or pipeline or loop" -q`
Expected: PASS(无回归)。

- [ ] **Step 9: Commit**

```bash
git add echo_agent/agent/loop.py echo_agent/agent/pipeline/context_stage.py tests/test_resource_leaks_batch2.py
git commit -m "快照缓存写入收口到 loop 统一 LRU,消除 _memory_snapshots 无界增长与双锁竞态"
```

---

### Task 4: trace 文件按数量上限轮转 + 配置字段

**Files:**
- Modify: `echo_agent/config/schema.py:2163`(ObservabilityConfig 新增字段)
- Modify: `echo_agent/observability/monitor.py:55-60,95-100`
- Modify: `echo_agent/agent/loop.py:179-182`(TraceLogger 构造传配置)
- Modify: `docs/config-reference.md` / `.yaml` / `.en.md` / `.en.yaml`(经生成器重生)
- Test: `tests/test_resource_leaks_batch2.py`(追加)

**Interfaces:**
- Consumes: 既有 `TraceLogger.flush_trace(trace_id)`(`monitor.py:95`)、`config.observability`(传入 loop)。
- Produces:
  - `TraceLogger.__init__(..., max_trace_files: int = 500)`、`self._max_trace_files`。
  - `TraceLogger._prune_trace_files() -> None` —— 按 mtime 升序删旧文件至上限;`max_trace_files <= 0` 不裁剪。
  - `ObservabilityConfig.max_trace_files: int = 500`(effective 字段)。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_resource_leaks_batch2.py 追加
def test_trace_files_pruned_to_limit(tmp_path):
    """flush 超过上限个 trace 后,目录只保留最近 N 个 trace_*.json。"""
    from echo_agent.observability.monitor import TraceLogger

    tracer = TraceLogger(logs_dir=tmp_path, enabled=True, max_trace_files=3)
    for i in range(5):
        tracer.start_span(trace_id=f"t{i}", span_id=f"sp{i}", name="x", kind="agent")
        tracer.flush_trace(f"t{i}")

    files = sorted(tmp_path.glob("trace_*.json"))
    assert len(files) == 3
    names = {f.name for f in files}
    assert "trace_t0.json" not in names  # 最旧被裁
    assert "trace_t4.json" in names      # 最新保留


def test_trace_prune_disabled_when_limit_non_positive(tmp_path):
    """max_trace_files <= 0 时不裁剪(禁用轮转),不误删。"""
    from echo_agent.observability.monitor import TraceLogger

    tracer = TraceLogger(logs_dir=tmp_path, enabled=True, max_trace_files=0)
    for i in range(4):
        tracer.start_span(trace_id=f"t{i}", span_id=f"sp{i}", name="x", kind="agent")
        tracer.flush_trace(f"t{i}")

    assert len(list(tmp_path.glob("trace_*.json"))) == 4
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_resource_leaks_batch2.py -k trace -v`
Expected: FAIL —— `TypeError: __init__() got an unexpected keyword argument 'max_trace_files'`。

- [ ] **Step 3: Add max_trace_files + prune to TraceLogger**

`echo_agent/observability/monitor.py` 的 `TraceLogger.__init__`(`monitor.py:55-61`)签名与赋值改为:

```python
    def __init__(self, logs_dir: Path | None = None, enabled: bool = True,
                 max_trace_files: int = 500):
        self._logs_dir = logs_dir
        self._enabled = enabled
        self._max_trace_files = int(max_trace_files)
        if logs_dir and enabled:
            logs_dir.mkdir(parents=True, exist_ok=True)
        self._traces: dict[str, list[TraceSpan]] = {}
        self._otel_tracer = None
```

把 `flush_trace`(`monitor.py:95-100`)改为写完后裁剪:

```python
    def flush_trace(self, trace_id: str) -> None:
        spans = self._traces.pop(trace_id, [])
        if self._enabled and self._logs_dir and spans:
            path = self._logs_dir / f"trace_{trace_id}.json"
            data = [s.to_dict() for s in spans]
            path.write_text(json.dumps(data, indent=2), encoding="utf-8")
            self._prune_trace_files()

    def _prune_trace_files(self) -> None:
        """按数量上限轮转 trace 文件:超过上限时按 mtime 升序删最旧的。
        best-effort——单个删除失败不影响主流程(trace 是 ephemeral 调试产物)。
        max_trace_files <= 0 视为禁用轮转。"""
        if self._max_trace_files <= 0 or not self._logs_dir:
            return
        try:
            files = sorted(
                self._logs_dir.glob("trace_*.json"),
                key=lambda p: p.stat().st_mtime,
            )
        except OSError:
            return
        excess = len(files) - self._max_trace_files
        for path in files[:max(0, excess)]:
            try:
                path.unlink()
            except OSError:
                continue
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_resource_leaks_batch2.py -k trace -v`
Expected: PASS(2 passed)。

- [ ] **Step 5: Add the config field**

在 `echo_agent/config/schema.py` 的 `ObservabilityConfig`(`schema.py:2163`),于 `trace_enabled` 字段(`schema.py:2172-2179`)之后新增:

```python
    max_trace_files: int = Field(
        default=500,
        json_schema_extra={
            "status": "effective", "ref": "observability/monitor.py:95",
            "desc_zh": "trace 文件保留数量上限,超出按最旧优先轮转删除;<=0 表示不限制(禁用轮转)",
            "desc_en": "Max retained trace files; oldest are rotated out when exceeded; <=0 disables rotation",
        },
    )
```

- [ ] **Step 6: Wire config into TraceLogger construction**

把 `echo_agent/agent/loop.py:179-182` 的 TraceLogger 构造改为:

```python
        self.tracer = TraceLogger(
            logs_dir=workspace / config.storage.logs_dir,
            enabled=config.observability.trace_enabled,
            max_trace_files=config.observability.max_trace_files,
        )
```

- [ ] **Step 7: Verify config metadata guard passes**

Run: `python -m pytest tests/test_config_metadata_guard.py tests/test_config_loader.py -v`
Expected: PASS —— 新字段元数据(status/ref/desc_zh/desc_en)完整,守卫通过。

- [ ] **Step 8: Regenerate config-reference docs**

Run: `python -m echo_agent config gen-docs`
然后确认四文件含新字段:
Run: `grep -rn "max_trace_files\|maxTraceFiles" docs/config-reference.md`
Expected: 命中一行(session/observability 段)。

- [ ] **Step 9: Commit**

```bash
git add echo_agent/observability/monitor.py echo_agent/config/schema.py echo_agent/agent/loop.py docs/config-reference.md docs/config-reference.yaml docs/config-reference.en.md docs/config-reference.en.yaml tests/test_resource_leaks_batch2.py
git commit -m "trace 文件按数量上限轮转,新增 observability.max_trace_files 配置(默认 500)"
```

---

### Task 5: 全量回归与 CI 口径校验

**Files:** 无代码改动(验证任务)。

- [ ] **Step 1: ruff(CI lint 口径)**

Run: `ruff check .`
Expected: `All checks passed!`。若报新错(如测试文件 E402 中段 import、F401 未用 import),就地修复后重跑。

- [ ] **Step 2: 全量测试(CI test 口径)**

Run: `python -m pytest tests/ -q`
Expected: 全绿(0 failed)。本批新增的 `test_resource_leaks_batch2.py` 四类测试全过,且无跨模块回归。

- [ ] **Step 3: 确认无依赖改动(security job 不受影响)**

Run: `git diff <本批起点>..HEAD --stat -- pyproject.toml`
Expected: 空(未动依赖,pip-audit 不受影响)。

---

## Self-Review

**Spec coverage(对照设计文档各节):**
- §2.1 expire_session load-on-miss → Task 1。✓
- §2.2 快照写入收口 loop LRU → Task 3(put_memory_snapshot + context_stage 回调)。✓
- §2.3 _evict_oldest 复用 _cleanup_deleted → Task 2。✓
- §2.4 trace 数量上限轮转 + 配置字段 → Task 4。✓
- §3 错误边界:2.1 load 仍 None 静默返回(Task1 Step3 代码含)、2.3 _cleanup_deleted 自吞异常(复用)、2.4 prune best-effort + `<=0` 禁用(Task4 两个测试覆盖)、2.2 put_snapshot 为空不回退无界直写(Task3 Step6 注释+局部变量)。✓
- §5 验证(ruff / 全量 pytest / 无依赖 / metadata guard)→ Task 4 Step7 + Task 5。✓
- §1 非目标:未触 DB logs 表死代码、未纳入相邻小泄漏、未动 default.yaml(Global Constraints 明确)。✓

**Placeholder scan:** Task 2 Step1 含两处"以 store.py 实际为准"的实现前核对指令(真实写入方法名 / embedding_id 字段名),并标注必须删除占位行 `store._evict_target`——这是有依据的对齐指令(store 写入入口需现场确认),非空洞占位;其余步骤均含可执行代码与命令。无 "TBD/handle edge cases"。

**Type consistency:** `put_memory_snapshot(key:str,value:str)->None`(async)、`put_snapshot: Callable[[str,str],Awaitable[None]]|None`、`_prune_trace_files()->None`、`max_trace_files:int`、`ObservabilityConfig.max_trace_files` 在 Task 3/4 间命名与签名一致。`_cleanup_deleted(evicted)`、`_load_from_storage(key)` 复用既有签名。

无遗留问题。
