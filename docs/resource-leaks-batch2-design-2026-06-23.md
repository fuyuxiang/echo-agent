# 第二批 · 长期运行资源泄漏修复 设计文档

- 日期：2026-06-23
- 类型：设计文档（spec），不含代码改动
- 上游依据：
  - `docs/architecture-remediation-roadmap-2026-06-23.md` 第二批 2.1–2.4
  - `docs/architecture-review-2026-06-22.md` 主线四（长期运行资源泄漏）
  - 记忆 `global-architecture-review-2026-06`、`remediation-batch1-done`
- 威胁模型：沿用 local-first / trusted-operator。本批四项均为 A 类真 bug（威胁模型无关，长期运行必然触发），直接提交 master，每项配隔离/回归测试。

---

## 1. 范围

严格限定 roadmap 第二批的四个泄漏点：

| # | 泄漏点 | 工作量 |
|---|---|---|
| 2.1 | `expire_session` 缓存未命中静默跳过 → sessions 表无界增长 | S |
| 2.2 | `_memory_snapshots` 不走 LRU、无上限 → 字典无界增长 | S |
| 2.3 | `_evict_oldest` 不清向量索引/SQLite 镜像 → FAISS 孤儿向量膨胀 | S |
| 2.4 | trace 文件每 turn 一个、无保留/清理 → logs 目录文件数无界增长 | S–M |

**非目标（防范围蔓延）：**
- 不纳入 review 列出的相邻同类小泄漏（TraceLogger `_traces` 异常路径泄漏、A2A `_tasks`、RateLimiter 桶字典、ProgressiveEditor 状态字典）——留 backlog。
- 不处理 DB `logs` 表死代码（属第三批"半成品诚实化"，非泄漏本身）。
- 不重构会话/记忆/可观测性的整体架构，只做定点止血。

## 2. 详细设计

### 2.1 expire_session 缓存未命中先 load 再落库

**问题**：`session/manager.py:294-300` 的 `expire_session` 仅在 `_cache.get(key)` 命中时改状态并 `save`。`cleanup_expired`（`manager.py:326`）遍历存储所有会话行逐个调它，但长驻进程里绝大多数行不在内存缓存 → 静默 no-op，过期行永远翻不到 `expired`/`archived`，`sessions` 表无界增长。

**改动**：`expire_session` 在缓存未命中时，先 `await self._load_from_storage(key)`（已存在，`manager.py:171`），拿到后改状态落库；仍找不到才返回。与同文件 `archive_session`（`manager.py:302-315`）已有的 load-on-miss 模式对齐——本质是把 `archive_session` 的正确写法补到 `expire_session`。

**改动落点**：`echo_agent/session/manager.py:294-300`。

**测试**：SQLite 模式造一个 `updated_at` 已超 `_expiry_delta` 的会话、不预热缓存 → 跑 `cleanup_expired` → 断言存储里该行 status 已落为 `expired`，返回计数为 1。

### 2.2 _memory_snapshots 写入收口到 loop 统一 LRU

**问题**：`agent/pipeline/context_stage.py:108-112` 直接 `self._memory_snapshots[key] = ...` + `move_to_end`，不走 `agent/loop.py:548 _lru_put`，无 popitem、无上限。对比 `_working_memories` 经 `_lru_put`（`loop.py:679`）受 `_max_cached_sessions=200` 约束，快照缓存无界增长。在 context_stage 内联 popitem 会绕开 `loop._state_lock`，造成同一 dict 双锁竞态。

**改动（方案 A，写入收口到 loop）**：
- `AgentLoop` 新增 async 方法 `put_memory_snapshot(key, value)`，内部 `await self._lru_put(self._memory_snapshots, key, value)`——复用已有锁 + `_max_cached_sessions` 上限。
- `ContextStage.__init__`（`context_stage.py:42-79`）：保留 `memory_snapshots: OrderedDict` 引用用于只读判断（`in` / `get`），**新增** `put_snapshot` 回调参数（async callable）。
- `ContextStage.build()`（async，`context_stage.py:93`）：`key not in self._memory_snapshots` 的读判断不变；写入由 `self._memory_snapshots[key] = ...` + `move_to_end` 改为单行 `await self._put_snapshot(key, snapshot)`。
- `AgentLoop` 构造 `ContextStage` 处（`loop.py:278-292`）传入 `put_snapshot=self.put_memory_snapshot`。

唯一写入入口经同一把锁和同一上限，消除竞态与无界增长。

**改动落点**：`echo_agent/agent/loop.py`（新增方法 + 构造传参）、`echo_agent/agent/pipeline/context_stage.py:42-79,108-112`。

**测试**：构造一个 stub，把真实 `_lru_put` 行为接到 `put_snapshot` 回调上（或直接对 `AgentLoop.put_memory_snapshot` 喂数据），写入超过 `_max_cached_sessions` 个不同 session_key → 断言 `_memory_snapshots` 大小不超上限、最旧 key 被逐出、最新保留。

### 2.3 _evict_oldest 同步清理向量索引 + SQLite 镜像

**问题**：`memory/store.py:480 _evict_oldest` 容量淘汰时只 pop entries + `_unindex_entry`（清内存倒排），不碰向量索引和 SQLite 镜像。对比 `delete()`（`store.py:611-629`）在 `_unindex_entry` 后调 `_cleanup_deleted(entry)`（`store.py:631`）清 FAISS + SQLite 镜像。容量淘汰路径走得越多，FAISS 孤儿向量与镜像行越膨胀。

**改动**：`_evict_oldest` 在 `self._entries.pop` + `_unindex_entry(evicted)` 之后，对被淘汰条目调 `self._cleanup_deleted(evicted)`。`_cleanup_deleted` 已是 best-effort（自带 running-loop 检测、task 管理、异常吞咽），无需新逻辑——只是把 delete 路径已有的清理接到 evict 路径。

**改动落点**：`echo_agent/memory/store.py:480-489`。

**测试**：构造带 mock `_vector_index`（记录 remove 调用）和 storage 的 store，写入条目使其超过 `_max_user`/`_max_env` 触发 `_evict_oldest` → 断言被淘汰条目的 embedding_id 进入了向量索引 remove 调用。

### 2.4 trace 文件按数量上限轮转

**问题**：`agent/loop.py:672 flush_trace` → `observability/monitor.py:95-100` 每 turn 写一个 `trace_{id}.json`，全程无 retention/rotate/prune。长驻进程下 logs 目录文件数无界增长。

**改动（方案 A，数量上限轮转）**：
- `TraceLogger.__init__`（`monitor.py:55`）新增 `max_trace_files: int = 500` 参数，存为 `self._max_trace_files`。
- `flush_trace`（`monitor.py:95`）写完新文件后调私有 `_prune_trace_files()`：`self._logs_dir.glob("trace_*.json")`，若数量超过 `_max_trace_files`，按 mtime 升序删最旧的若干个直到等于上限。best-effort，单个文件删除失败吞掉不影响主流程（trace 是 ephemeral 调试产物）。
- **可配置**：`ObservabilityConfig`（`schema.py:2163`）新增字段 `max_trace_files: int = 500`，带死字段治理要求的 `json_schema_extra`（status=effective / ref=`observability/monitor.py:95` 即 flush_trace 调 prune 处 / desc_zh / desc_en）；同步 `config/default.yaml`；经 `python -m echo_agent config gen-docs` 重生 config-reference 四文件。
- 构造 `TraceLogger` 的调用点把 `config.observability.max_trace_files` 传入。

**改动落点**：`echo_agent/observability/monitor.py:55-60,95-100`、`echo_agent/config/schema.py`（ObservabilityConfig）、`echo_agent/config/default.yaml`、`docs/config-reference.*`、`TraceLogger` 构造点（实现时定位）。

**测试**：用 tmp logs_dir 构造 `TraceLogger(max_trace_files=3)`，flush 5 个不同 trace_id（每个带至少一个 span）→ 断言目录里只剩 3 个 `trace_*.json` 且为最近的 3 个。

## 3. 错误边界与回退

- 2.1：`_load_from_storage` 仍返回 None（存储里也没有）时，保持原行为静默返回，不抛错。
- 2.3：`_cleanup_deleted` 内已吞咽向量/镜像清理异常，evict 主流程不受影响；无 storage 且无向量索引时早返回（已有逻辑）。
- 2.4：prune 删除失败 best-effort 吞咽；`max_trace_files <= 0` 视为不裁剪（禁用轮转），避免误删全部 trace。
- 2.2：回调未注入（None）时的兜底——实现时 context_stage 若 `put_snapshot` 为空则跳过缓存写入（只读 snapshot 不缓存），不得回退到无界 dict 写入。

## 4. 改动落点清单

- `echo_agent/session/manager.py`：`expire_session` load-on-miss。
- `echo_agent/agent/loop.py`：新增 `put_memory_snapshot`；构造 ContextStage 传 `put_snapshot`。
- `echo_agent/agent/pipeline/context_stage.py`：构造加 `put_snapshot` 回调；`build()` 写入改走回调。
- `echo_agent/memory/store.py`：`_evict_oldest` 调 `_cleanup_deleted`。
- `echo_agent/observability/monitor.py`：`TraceLogger` 加 `max_trace_files` + `_prune_trace_files`。
- `echo_agent/config/schema.py` + `default.yaml` + `docs/config-reference.*`：新增 `observability.max_trace_files`。
- 测试：四项各配隔离/回归测试（可集中一个 `tests/test_resource_leaks_batch2.py` 或就近归入现有测试文件，实现时定）。

## 5. 验证

- 单元/隔离测试：上述四项测试全绿。
- CI 口径（`.github/workflows/ci.yml`）：`ruff check .` 通过；`python -m pytest tests/` 全绿；未新增依赖，pip-audit 不受影响。
- 新配置字段须过 `tests/test_config_metadata_guard.py`（死字段治理元数据守卫）。

## 6. 工作量

四项均 S / S–M，落点集中、复用既有正确路径（archive_session / _lru_put / _cleanup_deleted / 既有 LRU 上限），不引入新抽象。
