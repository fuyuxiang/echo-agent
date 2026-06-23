# 第三批：半成品骨架"诚实化" — 设计文档

- 日期：2026-06-24
- 关联：`docs/architecture-review-2026-06-22.md`（主线五：能力声明 vs 落地）
- 前序批次：第一批真 bug、群聊会话隔离、第二批资源泄漏（均已完成）
- 威胁模型：trusted-operator（操作者可信，不防恶意本地插件）

## 目标

消除"宣称能力 ≠ 实际生效"的系统性落差。每个半成品要么**真生效**（接线），要么**诚实标注/移除**（降级），杜绝给读者与调用方虚假安全感。

## 统一原则

诚实化的本质：声明的能力 = 实际强制的现实。凡真正强制代价高、或有正确性/安全风险的，降级为 advisory 并明确文档化，而非补几个假装生效的调用做成"剧场"。叠加 trusted-operator 威胁模型与第一批锚定的"主线一：实现旁路"教训——不重造刚堵掉的旁路。

## 四组件处置总览

| 组件 | 处置 | 实质 |
|---|---|---|
| ContradictionDetector | 接线 | 唯一真做：保守自动消解 + 操作者复核入口 |
| InferenceController | 清理 | 删 3 个孤儿方法 + 1 个死字段；`validate_response` 明确为 advisory |
| WorkflowEngine | 诚实化 | 文档/工具描述标注"仅编排，执行需外部驱动 advance"；不接执行器 |
| PluginSandbox | 降级 | 运行时不可强制的权限类型降级为 advisory 元数据，删零调用方 check 方法 |

**明确不做**：工作流执行器（独立特性，且有重造第一批旁路的架构风险——执行器直跑工具会绕过 approval_gate / guards / path_policy / inference 约束）；sandbox 真隔离（需 OS 级 sandbox/子进程，trusted-operator 下不必要）；激进的 belief revision（误删正确记忆风险）。

## 组件一：ContradictionDetector — 接线（本批最实质）

**现状**：检测侧已闭环（写时 observe-only 打标 `store.py:556`；sleep 时 `consolidator.py:234` 跑 `check`→`store_contradiction` 落库）。`resolve(winner_id)` 已能调 `mark_superseded`（`contradiction.py:182`）让 retrieval 的 `is_superseded` 过滤生效（`retrieval.py:100`）。缺口仅两点：谁判定 winner 触发消解；操作者如何复核。`get_unresolved`/`get_history` 当前零调用方。

### (a) 自动消解：仅限"同 key + newest-wins"，仅在 sleep 时

在 `consolidator.sleep_consolidate` Step 3 的 `store_contradiction(c)` 之后增加受限消解判定：

- 只处理 `_heuristic_check` 类矛盾（`memory_id_a/b` 同 `key`、内容冲突）——语义明确：同一 key 不该有两个值。
- winner = `updated_at` 更新的一条（回退 `created_at`）；loser 调 `resolve(c.id, "a_wins"/"b_wins", winner_id=winner.id)`，复用既有路径 → `mark_superseded`。
- **明确排除**自动消解：LLM 检出的语义矛盾、temporal-conflict、跨 key(prefix) 矛盾——只 `store` 不消解，留人工。误删风险集中于此。
- 配置开关 `memory.auto_resolve_contradictions`，默认 **False**（诚实默认：不开则行为与现状一致，只检测不消解）。
- 计入 `stats`，新增 `resolved` 计数。

### (b) 人工复核入口：在现有 memory 工具上加只读/裁决 action

不新建工具，在现有 memory 工具上加：
- `list_contradictions` → 调 `get_unresolved`，操作者查看待裁决冲突。
- `resolve_contradiction(id, winner_id)` → 调已有 `resolve`，人工裁决。

二者一起做：只给列表不给裁决等于看得见改不了，闭环不完整。

## 组件二：InferenceController — 清理（无行为风险）

- 删 3 个孤儿方法（全项目零调用方）：`check_hallucination_markers`（几条粗正则、虚假安全感）、`build_verification_prompt`、`layer_system_prompts`。
- 删死字段 `InferenceConstraints.max_output_tokens`：inference_stage:618 实际用 `decision.max_tokens`（路由决策），此字段从不被读。删前确认无其他引用。
- `validate_response` 保留为 **advisory**：现状检出 issues 仅 `logger.warning`（inference_stage:328）。docstring 明确标注"软校验，仅告警不阻断"，消除误解。
- `filter_tools`（context_stage:133）、`needs_confirmation`（approval_gate:289）已真生效，不动。

## 组件三：WorkflowEngine — 诚实化（不接执行器）

**现状**：DAG 编排齐全（依赖解析、step→task 创建、状态机、cancel 级联），但 `TaskManager` 无执行器——queued task 把 `tool_name`/`tool_params` 存入 metadata 却无人执行，step 永远 PENDING，workflow 永不 advance 到 SUCCESS。

- `WorkflowTool.description`（`tools/workflow.py:14`）与 `WorkflowEngine` docstring 标注："仅编排 DAG 与步骤状态；步骤执行需外部驱动调用 `advance`，引擎自身不执行 step 的工具。"
- `advance` action 的 description 点明"由外部驱动推进"。
- 不动状态机/DAG 逻辑（本身正确）。不加执行器。

## 组件四：PluginSandbox — 降级（消除虚假安全感）

**核心事实**：in-process Python 插件，`check_network`/`check_subprocess`/`check_filesystem_*` 靠插件自觉调用，对恶意代码零约束，且全项目零运行时调用方。trusted-operator 下本不防恶意插件。

- `network`/`subprocess`/`filesystem.read`/`filesystem.write` 从"强制权限"语义降级为**声明性 manifest 元数据**；模块注释/docstring 标注"advisory，用途透明化，非安全边界，运行时不强制"。
- 删 4 个零调用方且无法真强制的运行时方法：`check_network`/`check_subprocess`/`check_filesystem_read`/`check_filesystem_write`。
- 保留 `check_tool_register`/`check_hook_register`——在 manager:164-165 注册期真生效。
- `PluginManifest.permissions` 字段保留（声明价值），文档说明其 advisory 性质。

## 测试与验证

新增 `tests/test_honest_capabilities_batch3.py`：

- **矛盾自动消解**：同-key newest-wins 触发 `mark_superseded`、retrieval 过滤生效；开关默认 False 时不消解；LLM/temporal/跨-key 矛盾不被自动消解。
- **人工复核**：`list_contradictions` 返回未决；`resolve_contradiction` 裁决后 loser 被 supersede。
- **清理类**：导入/grep 断言确认死方法与死字段已删、无残留引用。

验证基线（沿用前两批）：TDD；全量 `python -m pytest tests/` 0 fail；`ruff check .` 过；无依赖改动；CI 三 job 本地验证口径。master 直接提交，commit 不带前缀、无 Claude 署名。

## 风险与权衡

- 自动消解默认关闭，且只碰语义最明确的同-key 冲突，最大限度规避误删正确记忆。
- 降级而非删除 `permissions` 字段，保留声明透明度，但通过文档断绝"它在强制"的误解。
- 工作流不接执行器是有意决策：避免在 trusted 边界内重造免审批的工具执行旁路。

