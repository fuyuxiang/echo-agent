# 写入时矛盾检测：从「自动取代」收敛为「只观测、只标记」

- 日期：2026-06-08
- 状态：设计待评审
- 范围：`contradiction_scan_on_store` 特性的重定位 + 一处正确性 bug 修复 + 一处接线补全
- 不在范围内：gate/EvolutionConfig 接线缺口、ContextCache 死代码（这两项作为后续独立修复，与本特性无关）

## 1. 背景与动机

本批改动新增了 `contradiction_scan_on_store` 开关：在记忆写入（`MemoryStore.add()`）时同步运行一次轻量矛盾扫描，命中「同 key 前缀 + 内容不同 + 时间差 ≥ 1 天」时，自动把旧条目标记为 `superseded_by = 新条目`。

经过对当前记忆架构的完整梳理与 2026 年业界记忆架构实践的对照，这个「写入时自动取代」的方向与项目自身哲学、业界共识三方冲突，需要重定位。

### 1.1 当前已有的四条冲突处理路径

| 路径 | 时机 | 方法 | 可靠性 | 覆盖范围 |
|---|---|---|---|---|
| 1. 同 key 合并 (`_find_conflict`/`_merge_locked`) | `add()` 同步 | key 精确匹配 → 就地覆盖 content | 高（确定性） | 同 key 事实更新 |
| 2. 写入时轻量扫描 (`contradiction_scan_on_store`) | `add()` 同步 | 启发式 + 1 天硬阈值 → **自动 supersede** | 中（无 LLM、僵硬） | 同 key + 时间冲突 |
| 3. 整合时完整检测 (`detector.check`) | consolidate 异步 | 向量预筛 + **LLM 判定** → 仅 `store_contradiction`，**不自动 resolve** | 高 | 全语义空间 |
| 4. 版本链 + 检索过滤 (`supersede`/`is_superseded`) | resolve 后 | 标记 superseded，检索/写入候选自动跳过 | 高 | 善后 |

关键观察：路径 2（本特性）夹在可靠的路径 1 与路径 3 之间，功能区间狭窄且方法错配——同 key 更新已由路径 1 确定性处理；真正的语义冲突需要路径 3 的 LLM。路径 2 用启发式 + 硬阈值去做一件「要么 key 能精确匹配、要么需要语义理解」的事。

### 1.2 与项目哲学冲突

记忆系统上一次重构（commit e3d3db5）确立的核心信条：

> 「可靠地记住关于用户的事，优先于聪明地检索任何事。个人助理的记忆服务于关系，而非知识库。」

该重构的全部动作（USER 记忆不衰减、溢出给策展信号而非静默丢弃、消除自相矛盾的可变记忆、修「昨天说的生日今天问不出」）都指向克制、保守、不静默丢失。「写入时用启发式自动取代用户事实」与此正面冲突。

### 1.3 与 2026 业界共识冲突

- mem0《2026 记忆架构现状》："Most systems treat change as replacement. The right behavior treats it as evolution."（多数系统把变化当替换，正确做法是当演化）。mem0 已于 2026-04 废弃 ADD/UPDATE/DELETE/NOOP 多操作模型，改为 **Single-pass ADD-only extraction**——放弃写入时自动删改，把取舍交给检索期多信号排序。写入默认异步。并将「memory staleness」列为未解开放难题，承认「高相关度记忆的过时是更难的开放问题」。
- BeliefShift 基准：失败模式是双向权衡——「激进个性化模型抗漂移差（易被轻易覆盖），事实接地强的模型会错过合理更新（过度坚持）」。「轻易覆盖旧事实」与「过度坚持」都被当作失败惩罚。0.85 相似度 + 时间差的启发式自动取代，正是滑向「激进覆盖」一端。

三方收敛：项目哲学、生产实践、学术结论，都指向同一结论——**不要在写入时用启发式自动取代用户事实**。

## 2. 设计目标

把 `contradiction_scan_on_store` 从「裁决者」重定位为「观测者」：

1. 写入时只负责「如实记下 + 标出可疑」，绝不做物理取代。
2. 「谁该被取代」的语义裁决，完整交还给路径 1（确定性合并）与路径 3（LLM 整合）。
3. 顺带修复一处真实的 supersede 方向 bug（防御性，无论开关是否启用都该修）。
4. 补上 `config → MemoryStore` 接线缺口，但默认保持关闭，定位为「诊断用可观测开关」。

## 3. 详细设计

### 3.1 改动点 A：移除自动 supersede，改为只标记

`store.py` 的 `_run_contradiction_scan(entry)`：

- 移除 `older.superseded_by = entry.id` 这一行（及相关 `_dirty_ids` 写入）——写入路径不再产生任何物理取代。
- 命中疑似冲突时，改为在**旧条目**的 `tags` 上追加软标记 `suspected_conflict`（经 `_normalize_tags` 去重），并记 `logger.info`。
- `check_lightweight_sync` 返回的 `Contradiction` 仅用于日志与标记，不再驱动状态变更。

效果：写入只「标出可疑」，不「裁决」。对齐 mem0「change as evolution, not replacement」与项目「不静默丢弃」。

### 3.2 改动点 B：修复 supersede 方向 bug（防御性）

`_temporal_conflict_check` 与任何消费其结果的代码中，`memory_id_a` 被约定为 older、`memory_id_b` 为 newer（由检测器按时间戳算出）。原 `_run_contradiction_scan` 假设「新写入的 `entry` 一定是 newer」，在补录历史或时钟漂移时会把方向判反。

由于改动点 A 已移除写入时的 supersede，此 bug 在写入路径上不再触发；但为防御路径 3/4（整合器或外部 resolve 复用同一判定结果），需保证 `_temporal_conflict_check` 返回的 `memory_id_a`/`memory_id_b` 严格按「检测器算出的 older/newer」赋值，不依赖「哪个是新写入的」。补充单测覆盖「新写入条目时间戳反而更旧」的场景。

### 3.3 改动点 C：补全 config → MemoryStore 接线

`loop.py` 实例化 `MemoryStore` 时传入 `contradiction_scan_on_store=config.memory.contradiction_scan_on_store`，使配置项可被显式开启。默认保持 `False`。开启后行为为「只标记不取代」（改动点 A 后），定位为诊断/可观测开关，绝不默认改变记忆状态。

### 3.4 标记如何被消费（与路径 3 衔接）

整合器 `sleep_consolidate` 的 Step 3 已对新提升条目调用 `detector.check`（LLM 判定）。`suspected_conflict` 标记作为**优先复核信号**：整合器可优先把带此标记的条目纳入 `check` 的候选，从而把「写入时粗筛」与「整合时精判」串成接力。本 spec 不强制改造整合器的候选选择逻辑（YAGNI），仅保证标记存在且语义清晰，供其后续消费。

## 4. 数据流

```
add(entry) [同步]
  ├─ _find_conflict → 同 key 命中则 _merge_locked（路径 1，不变）
  ├─ _queue_embed(entry)
  └─ if contradiction_scan_on_store:        # 默认 False
       _run_contradiction_scan(entry)
         └─ check_lightweight_sync → 命中疑似冲突
              └─ 旧条目 tags += "suspected_conflict" + logger.info
                 （不写 superseded_by）

sleep_consolidate [异步，定期]
  └─ detector.check(LLM)（路径 3，不变）
       └─ store_contradiction（记录，不自动 resolve）
```

写入路径不再有任何「删除/取代」语义；唯一的状态变更是给旧条目打标记。

## 5. 错误处理

- `_run_contradiction_scan` 整体包在防御性 try/except 中（沿用现有风格），扫描失败只记 `logger.debug`，绝不影响 `add()` 主流程返回。
- 标记写入复用 `_normalize_tags` 保证幂等：重复命中不会产生重复标记。
- 开关关闭（默认）时，整条扫描路径不执行，零开销、零行为变更。

## 6. 测试计划

- 单测：开关开启时，命中疑似冲突 → 旧条目获得 `suspected_conflict` 标记，且 `superseded_by` 保持为空（验证「不取代」）。
- 单测：`_temporal_conflict_check` 在「新写入条目时间戳更旧」时，`memory_id_a`=较旧者、`memory_id_b`=较新者（验证方向修复）。
- 单测：开关关闭（默认）时，`add()` 行为与改动前完全一致（无标记、无副作用）。
- 单测：同一冲突重复触发 → 标记不重复（幂等）。
- 回归：现有 `tests/test_contradiction_advanced.py` 中依赖旧「自动 supersede」语义的用例需相应更新为「只标记」。

## 7. 验证

- 运行 `tests/test_contradiction_advanced.py` 及记忆相关全套测试，确认通过。
- 确认 `loop.py` 接线后，配置开启时标记生效、关闭时无副作用。

## 8. 被显式排除的方案（及理由）

- **写入时同步向量相似度自动取代**：`add()` 同步且新条目此刻尚无 embedding（`_queue_embed` 仅排队），无法在同步上下文算相似度。
- **异步 flush 后做 top-K 相似度自动取代（曾作为方案 2）**：可实现，但为「不该自动化的删除决定」搭建过度工程，且与三方共识（项目哲学 / mem0 / BeliefShift）相悖。
- **将 `add()` 整体异步化**：改动面巨大（reviewer/consolidator/tiers 多处同步调用方），违背「聚焦本特性、不做无关重构」。

