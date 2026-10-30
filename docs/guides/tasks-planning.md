# 任务与规划

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

Echo Agent 提供完整的任务生命周期管理：从轻量的 Todo 清单到 DAG 驱动的多步 Workflow，再到多 Agent 协同的 delegate 分派。所有任务统一呈现在 Dashboard Kanban 看板上，支持实时监控和手动干预。

---

## 核心概念

| 概念 | 工具 | 用途 |
|------|------|------|
| **Todo** | `todo` | 轻量任务清单，per-session 存储，适合规划阶段的思路梳理 |
| **Task** | `task` | 持久化任务记录，具备完整状态机、优先级、看板追踪 |
| **Workflow** | `workflow` | DAG 多步编排，自动解析依赖并按序调度 step 任务 |
| **Delegate** | `delegate_task` | 多 Agent 分派，主 Agent 分拆子任务交给 worker 并行执行 |

---

## 任务状态机

Task 的核心是一个严格的有限状态机，所有状态转换必须经过校验。

### 状态定义

| 状态 | 含义 | 终态 |
|------|------|------|
| `PENDING` | 已创建，等待入队 | |
| `QUEUED` | 已入队，等待执行 | |
| `RUNNING` | 正在执行 | |
| `BLOCKED` | 被外部依赖阻塞 | |
| `REVIEW` | 执行完毕，等待审核 | |
| `SUSPENDED` | 手动挂起 | |
| `SUCCESS` | 成功完成 | Yes |
| `FAILED` | 执行失败 | Yes |
| `CANCELLED` | 已取消 | Yes |

### 状态转换图

```
PENDING ──→ QUEUED ──→ RUNNING ──→ REVIEW ──→ SUCCESS
  │            │          │  │  │               │
  │            │          │  │  └→ SUSPENDED    │
  │            │          │  └──→ BLOCKED       │
  │            │          └────→ FAILED         │
  │            │                    │           │
  │            ←────────────────────┘ (retry)   │
  │            ←────────────────────────────────┘ (reject → re-queue)
  │            ←──── BLOCKED (unblock)
  │            ←──── SUSPENDED (resume)
  └──→ CANCELLED ← (可从任何非终态到达)
```

### 合法转换表

```python
VALID_TASK_TRANSITIONS = {
    PENDING:   {QUEUED, CANCELLED},
    QUEUED:    {RUNNING, CANCELLED},
    RUNNING:   {REVIEW, BLOCKED, FAILED, SUSPENDED, CANCELLED},
    BLOCKED:   {QUEUED, RUNNING, CANCELLED},
    REVIEW:    {SUCCESS, QUEUED, CANCELLED},
    SUSPENDED: {QUEUED, RUNNING, CANCELLED},
    FAILED:    {QUEUED},         # retry
    SUCCESS:   {},               # 终态
    CANCELLED: {},               # 终态
}
```

!!! warning "状态转换规则"
    - 不在上表中的转换会抛出 `ValueError`，TaskManager 会拒绝该操作。
    - Agent 调用 `task complete` 时，工具内部自动补 `RUNNING → REVIEW → SUCCESS` 两步转换，无需手动经过 REVIEW。
    - `FAILED → QUEUED` 是唯一的重试路径，每次重试 `retry_count` 递增，超过 `max_retries` 后不再允许。

---

## 任务生命周期

### 创建任务

```json
{"tool": "task", "action": "create", "title": "爬取文档", "priority": 3}
```

新任务初始状态为 `PENDING`，自动分配唯一 ID（格式 `t_xxxxxxxxxxxx`）。

### 启动与执行

```json
{"tool": "task", "action": "start", "task_id": "t_abc123"}
```

状态经 `PENDING → QUEUED → RUNNING` 流转。Agent 在 `RUNNING` 期间执行实际工作。

### 完成或失败

```json
{"tool": "task", "action": "complete", "task_id": "t_abc123", "result": "爬取了 42 篇文档"}
{"tool": "task", "action": "fail", "task_id": "t_abc123", "error": "目标站点 503"}
```

### 重试

```json
{"tool": "task", "action": "retry", "task_id": "t_abc123"}
```

将 `FAILED` 任务重新入队，`retry_count` 加 1。

### 阻塞与挂起

任务向用户追问时进入 `BLOCKED`，保留原会话和计划，不推进工作流。在看板任务详情中选择选项或填写回答，再点「提交回答并继续」，任务会排队续接；回答失败时输入仍保留。重复提交、过期问题及已取消任务不能重新启动执行。该操作要求管理员权限，回答不会授予危险工具的执行权限。

聊天中关联任务的追问仍在原聊天回答；发送失败时可在「通道 → 消息投递恢复」先恢复问题。后台看板任务的最终正文写回「执行结果」，无需依赖聊天发送通道。

接口：`POST /api/v1/tasks/{id}/clarification`，请求包含 `question_id` 和非空 `answer`（最多 10000 字符），前缀遵循 Gateway 配置。

- **BLOCKED**：任务因外部依赖无法继续，设置 `blocked_reason` 说明原因。
- **SUSPENDED**：手动暂停，可随时恢复到 `QUEUED` 或 `RUNNING`。

---

## TaskRecord 字段说明

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | string | 任务 ID，自动生成 |
| `workflow_id` | string | 所属 workflow（空表示独立任务）|
| `parent_task_id` | string | 父任务 ID（用于层级分解）|
| `board_id` | string | 看板 ID，默认 `"default"` |
| `title` | string | 任务标题 |
| `description` | string | 详细描述 |
| `status` | TaskStatus | 当前状态 |
| `priority` | int (0-9) | 优先级，0 最高，默认 5 |
| `labels` | list[str] | 标签列表 |
| `assignee` | string | 分配人/Agent |
| `source` | string | 来源标识 |
| `session_id` | string | 关联会话 |
| `blocked_reason` | string | 阻塞原因 |
| `review_summary` | string | 审核摘要 |
| `result` | string | 执行结果 |
| `error` | string | 错误信息 |
| `retry_count` | int | 已重试次数 |
| `max_retries` | int | 最大重试次数，默认 3 |
| `metadata` | dict | 自定义元数据（workflow step 信息等）|

---

## Workflow 编排

Workflow 是基于 DAG（有向无环图）的多步骤编排引擎。每个步骤定义依赖关系，引擎自动解析拓扑序并调度就绪的步骤。

### Workflow 状态机

```
PENDING ──→ RUNNING ──→ SUCCESS
  │            │  │
  │            │  └→ WAITING (暂停等待)
  │            │  └→ BLOCKED
  │            └──→ FAILED (terminal; inspect results before creating new work)
  └──→ CANCELLED ← (可从任何非终态到达)
```

### 创建 Workflow

```json
{
  "tool": "workflow",
  "action": "create",
  "name": "数据处理流水线",
  "steps": [
    {"id": "fetch", "name": "抓取数据", "tool_name": "web_fetch", "tool_params": {"url": "https://example.com"}},
    {"id": "parse", "name": "解析数据", "tool_name": "knowledge_index", "tool_params": {"action": "status"}, "depends_on": ["fetch"]},
    {"id": "store", "name": "存储结果", "tool_name": "write_file", "tool_params": {"path": "report.txt", "content": "Replace with the verified result"}, "depends_on": ["parse"]}
  ]
}
```

### StepDefinition 字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | string | 步骤 ID（未指定则自动生成 `step_0`, `step_1`...）|
| `name` | string | 步骤名称 |
| `tool_name` | string | 要执行的工具名 |
| `tool_params` | dict | 工具参数 |
| `depends_on` | list[str] | 依赖的步骤 ID 列表 |
| `condition` | string | 预留字段，只接受空字符串；非空条件明确报错，不会静默忽略 |
| `retry_max` | int | 非负整数，步骤失败后自动重试的次数，默认 0；重试可能重复外部副作用，应仅对可安全重试的步骤启用 |
| `timeout_seconds` | int | 正整数，从 Runtime 分发该步骤开始计时，默认 300 秒；包含排队与审批等待 |

创建和启动前会拒绝空图、重复步骤 ID、缺失依赖与循环依赖。并发 `advance` 不会重复创建步骤任务；任务创建后、工作流映射保存前中断留下的 PENDING 步骤，会在下一次启动/推进时接续。

### 执行模型

Workflow 引擎**只做编排**——它解析依赖、创建步骤任务、推进状态，但**不执行工具**。实际执行由 Agent 完成：

1. `workflow start` → 引擎将无依赖的步骤创建为 `QUEUED` 任务
2. Runtime 的 TaskDispatcher 自动领取就绪步骤，在独立的 `task:<id>` 会话中交给 Agent（`metadata` 中含 `tool_name`/`tool_params`）
3. Agent 按步骤指令调用工具并通过 `task complete` / `task fail` 结束任务；Runtime 也会依据回合实际结果补写终态
4. `task complete` 自动触发 `workflow.advance()`，引擎检查依赖并调度后续步骤
5. 所有步骤成功才会成为 `SUCCESS`；失败步骤在 `retry_max` 耗尽后使 workflow 成为 `FAILED`；取消步骤使 workflow 成为 `CANCELLED`，不会记为成功

暂停会阻止尚未领取的步骤及后继步骤开始，已经领取的步骤允许完成；恢复时先核对其真实结果。步骤超时会记录失败并向该回合发出协作中断；运行中的工具可能到下一检查点才停止，已经发生的文件或外部副作用不会回滚。旧 attempt 的迟到结果不能覆盖重试后的新 attempt。

!!! note "advance 是 best-effort，不是事务的一部分"
    `task complete`、`task fail` 与 `task cancel` 都会在状态落库**之后**触发一次 advance。

    任务状态的持久化与 workflow 的推进是两步，前者先完成。advance 失败时只记录一条告警，不会让工具调用失败、也不会回滚任务状态——代价是 DAG 的进度暂时停在原处，直到下一次显式执行 `workflow advance` 补上。系统没有后台补偿任务来兜这件事。

    因此长时间无进展的 workflow 值得手动 `workflow status` 检查一次：任务已是终态而 workflow 仍停在旧步骤，就是 advance 丢过一次。

### Workflow 操作

| 操作 | 说明 |
|------|------|
| `start` | 启动 workflow，调度就绪步骤 |
| `status` | 查看 workflow 当前状态和步骤进度 |
| `advance` | 手动推进（正常流程自动推进，此操作用于手动修复）|
| `pause` | 暂停 workflow（状态→WAITING）|
| `resume` | 恢复 workflow，重新调度就绪步骤 |
| `cancel` | 取消 workflow 及其所有未完成步骤任务 |
| `list` | 列出所有 workflow，可按状态过滤 |

---

## Todo 工具

`todo` 是轻量级的任务清单工具，数据按 session 存储在本地 JSON 文件中。适合 Agent 在规划阶段组织思路、拆解步骤。

### 操作

```json
{"tool": "todo", "action": "create", "title": "调研竞品 API"}
{"tool": "todo", "action": "create", "items": [
  {"title": "第一步：读取配置"},
  {"title": "第二步：验证参数"},
  {"title": "第三步：执行迁移"}
]}
{"tool": "todo", "action": "list"}
{"tool": "todo", "action": "update", "task_id": "t_abc123", "status": "in_progress"}
{"tool": "todo", "action": "complete", "task_id": "t_abc123"}
{"tool": "todo", "action": "delete", "task_id": "t_abc123"}
```

### Todo vs Task

| 维度 | Todo | Task |
|------|------|------|
| 持久化 | 本地 JSON，session 粒度 | 数据库持久化 |
| 状态机 | `pending` / `in_progress` / `done` | 9 状态严格状态机 |
| 看板 | 不在 Kanban 上显示 | 展示在 Dashboard Kanban |
| Workflow | 不支持 | 支持作为 workflow step |
| 用途 | 规划、思考、临时清单 | 正式任务跟踪与调度 |

---

## 多 Agent 分派（Delegate）

`delegate_task` 工具让主 Agent（编排者）将子任务分派给 Worker Agent 并行执行。

### 调用方式

```json
{
  "tool": "delegate_task",
  "goal": "将项目根目录 README.md 翻译为日语，保持格式不变"
}
```

### 执行模型

1. 主 Agent 调用 `delegate_task`，描述子任务和上下文
2. 系统创建 Worker Agent，分配受限工具集（不含 `delegate_task`/`spawn_task`/`clarify` 等）
3. Worker 独立执行，受 `max_iterations` 和 `timeout_seconds` 约束
4. Worker 返回结构化结果给主 Agent
5. 主 Agent 综合各 Worker 结果继续推进

### 安全约束

- Worker 不可调用 `delegate_task`（防止递归分派风暴）
- Worker 不可直接向用户发消息（`message`/`notify` 被屏蔽）
- Worker 的工具调用受主 Agent 同等的审批策略约束
- 每个 Worker 有独立的 `trace_id` 用于审计追踪

!!! info "风险等级"
    `delegate_task` 工具的 `risk_level` 为 `exec`，表示它会执行实际操作。在需要审批的通道中，分派本身需要先通过审批门控。

---

## Kanban 看板

### Dashboard Kanban 页面

Dashboard 提供可视化 Kanban 看板，按任务状态分列展示：

```
┌─────────┬─────────┬─────────┬─────────┬─────────┬─────────┐
│ PENDING │ QUEUED  │ RUNNING │ REVIEW  │ BLOCKED │ FAILED  │
├─────────┼─────────┼─────────┼─────────┼─────────┼─────────┤
│ Card    │ Card    │ Card    │ Card    │         │ Card    │
│ Card    │         │ Card    │         │         │         │
└─────────┴─────────┴─────────┴─────────┴─────────┴─────────┘
```

### 看板特性

- **常驻列**：PENDING、QUEUED、RUNNING、REVIEW、BLOCKED、FAILED 始终显示
- **终态归档**：SUCCESS 和 CANCELLED 任务不在主视图显示（可切换查看）
- **卡片信息**：标题、优先级、标签、分配人、来源、阻塞原因
- **状态配色**：每个状态有独立的视觉配色方案
- **拖拽操作**：支持在合法转换范围内拖拽卡片改变状态
- **Board 隔离**：通过 `board_id` 支持多看板，默认看板 ID 为 `"default"`

### 通过 API 访问

```
GET  /api/v1/tasks?board_id=default
GET  /api/v1/tasks?status=running
POST /api/v1/tasks/{id}/transition  {"status": "queued"}
```

---

## 优先级与配置

### 优先级系统

优先级范围 0-9，数字越小优先级越高：

| 范围 | 含义 | 使用场景 |
|------|------|----------|
| 0-2 | 紧急 | 用户直接指令、阻塞性问题 |
| 3-4 | 高 | 重要功能、限时任务 |
| 5 | 默认 | 常规任务 |
| 6-7 | 低 | 优化、非紧急改进 |
| 8-9 | 最低 | 后台清理、实验性任务 |

### 并发与重试

- `max_retries`：最大重试次数，默认 3
- `retry_count`：已重试次数，每次 `retry` 操作加 1
- 乐观锁（`version` 字段 + CAS）：防止并发更新冲突
- 租约机制（`lease_until_ms`）：防止 Worker 崩溃后任务永久卡在 RUNNING

---

## 使用场景示例

### 长会话与澄清

长会话压缩只有获得完整、可用的摘要后才替换历史。摘要模型失败、输出截断、内容过滤或处于失败冷却期时，原始历史与媒体引用会保留，不会把缺少中段记录的结果保存为压缩成功。若保留的历史仍超过模型窗口，本次请求可能返回上下文限制错误；恢复模型后可重试，或显式开启新会话。

通过 CLI attach 收到澄清提示后可以立即回答；提示仍在投递时收到的答案也会保留到工具读取。断开或取消会话仍按既有中断流程结束等待。

### 场景 1：简单任务跟踪

```
User: 帮我把这个 bug 修了
Agent:
  → task create "修复登录页 500 错误" priority=2
  → task start
  → (执行修复)
  → task complete result="修复了空指针异常，已添加测试"
```

### 场景 2：多步骤流水线

```
Agent:
  → workflow create "部署流水线" steps=[
      {id: "test", tool_name: "exec", tool_params: {command: "pytest"}},
      {id: "build", tool_name: "exec", tool_params: {command: "docker build"}, depends_on: ["test"]},
      {id: "deploy", tool_name: "exec", tool_params: {command: "kubectl apply"}, depends_on: ["build"]}
    ]
  → workflow start
  → (引擎自动调度 test → build → deploy)
```

### 场景 3：并行分派

```
Agent:
  → delegate_task goal="将 CHANGELOG 翻译为日语"
  → delegate_task goal="将 CHANGELOG 翻译为韩语"
  → (两个 Worker 并行执行，主 Agent 汇总结果)
```

### 场景 4：规划阶段用 Todo

```
Agent:
  → todo create items=[
      {title: "分析现有数据模型"},
      {title: "设计新的 API 接口"},
      {title: "编写迁移脚本"},
      {title: "编写测试用例"}
    ]
  → (逐步执行，标记完成)
  → todo complete task_id="t_xxx"
```
