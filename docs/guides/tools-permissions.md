# 工具使用与权限控制

Echo Agent 通过可扩展的工具体系为大模型赋予操作外部世界的能力。每个工具都有明确的风险等级和能力声明，运行时由审批网关（Approval Gate）决定是否放行。

---

## 工具基类

所有工具继承自公开入口 `echo_agent.tools.Tool`，核心属性如下：

| 属性 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `name` | `str` | `""` | 工具唯一标识 |
| `description` | `str` | `""` | 功能描述（注入模型上下文） |
| `parameters` | `dict` | `{}` | JSON Schema 参数定义 |
| `timeout_seconds` | `int` | `30` | 单次执行超时 |
| `max_retries` | `int` | `0` | 失败重试次数 |
| `stream_capable` | `bool` | `False` | 是否支持流式输出 |
| `capabilities` | `tuple[str, ...]` | `()` | 能力声明（覆盖静态映射） |
| `risk_level` | `str` | `"write"` | 风险等级 |

工具还提供以下运行时检查方法：

- `is_ready() -> bool`：外部依赖是否就绪（如 API Key 已配置）
- `readiness_detail() -> tuple[bool, str]`：详细就绪状态与原因

---

## 完整工具目录

### 只读工具（read_only）

| 工具名 | 说明 | 能力 |
|--------|------|------|
| `read_file` | 读取文件内容 | `fs.read` |
| `list_dir` | 列出目录内容 | `fs.read` |
| `search_files` | 搜索文件内容 | `fs.read` |
| `knowledge_search` | 知识库检索 | `knowledge.read` |
| `session_search` | 搜索历史会话 | `session.read` |
| `skills_list` | 列出已安装技能 | `skill.read` |
| `skill_view` | 查看技能详情 | `skill.read` |
| `agents_list` | 预留名称，当前未实现/不可调用 | — |
| `agents_route` | 预留名称，当前未实现/不可调用 | — |
| `web_fetch` | 抓取网页内容 | `network.outbound` |
| `web_search` | 网络搜索 | `network.outbound` |
| `vision_analyze` | 图像分析 | `media.read` |
| `read_spill` | 读取溢出内容 | `fs.read` |

### 写入工具（write）

| 工具名 | 说明 | 能力 |
|--------|------|------|
| `write_file` | 写入文件 | `fs.write` |
| `edit_file` | 编辑文件 | `fs.read`, `fs.write` |
| `patch` | 补丁式修改文件 | `fs.read`, `fs.write` |
| `knowledge_index` | 构建知识库索引 | `knowledge.write`, `fs.read` |
| `todo` | 任务清单管理 | `task.write` |
| `task` | 任务创建与调度 | `task.write` |
| `workflow` | 工作流编排 | `workflow.write` |
| `notify` | 发送通知 | `message.send` |
| `message` | 发送消息 | `message.send` |
| `clarify` | 向用户提问澄清 | `message.ask` |
| `memory` | 记忆读写 | `memory.read`, `memory.write` |
| `image_generate` | 生成图像 | `media.generate`, `network.outbound` |
| `text_to_speech` | 文字转语音 | `media.generate`, `network.outbound` |

### 执行工具（exec）

| 工具名 | 说明 | 能力 |
|--------|------|------|
| `exec` | 执行进程/命令 | `process.exec` |
| `execute_code` | 执行代码片段 | `code.exec`, `process.exec` |
| `process` | 进程管理（启动/信号/stdin） | `process.exec`, `process.manage` |
| `delegate_task` | 委派任务给子代理 | _继承子代理能力_ |
| `spawn_task` | 后台创建子代理任务 | _继承子代理能力_ |

### 危险工具（dangerous）

| 工具名 | 说明 | 能力 |
|--------|------|------|
| `cronjob` | 创建/管理定时任务 | `scheduler.write` |
| `skill_install` | 安装外部技能包 | `skill.install`, `network.outbound`, `fs.write` |
| `skill_manage` | 管理/删除技能 | `skill.write`, `fs.write` |

---

## 风险等级

系统定义四个风险等级，由 `RiskLevel` 枚举表示：

| 等级 | 值 | 审批要求 | 说明 |
|------|-------|----------|------|
| 只读 | `read_only` | 无需审批 | 纯读取操作，无副作用 |
| 写入 | `write` | 交互模式下自动放行 | 有副作用但受沙箱/路径策略保护 |
| 执行 | `exec` | 需白名单或审批 | 进程/代码执行，可能产生任意副作用 |
| 危险 | `dangerous` | 始终需要人工审批 | 创建持久性特权状态（定时任务/技能安装） |

风险判定取两个来源中较严格的：

1. **静态映射**（`_TOOL_RISK_MAP`）：按工具名硬编码
2. **工具声明**（`Tool.risk_level`）：工具类自身声明

```python
# 分类逻辑：取两者最大严格度
risk = max(static_risk, declared_risk, key=severity)
```

!!! warning "风险升级原则"
    当静态映射与工具声明不一致时，系统取更严格者。这意味着一个声明为 `exec` 的 MCP 工具不会因缺少静态映射而被降级为 `write`。

---

## 审批流程

工具调用经过 `ApprovalGate.check()` 的多步决策：

```
┌─ Step 1: 静态安全守卫（GuardDecision）
│   ├─ deny → 直接拒绝（不可逆的破坏性命令）
│   └─ ask/allow → 继续
│
├─ Step 2: 提权检查
│   └─ 安全策略要求提权但来源无权 → 拒绝
│
├─ Step 3: 风险分级
│   └─ classify_risk(tool_name, args, declared_risk)
│
├─ Step 4: 无人值守判断
│   ├─ unattended + READ_ONLY → 放行
│   ├─ unattended + cron_authorized + WRITE/EXEC → 放行
│   ├─ unattended + DANGEROUS → 拒绝
│   └─ unattended + 其他 → 按 unattended_policy 决策
│
├─ Step 5: READ_ONLY / WRITE → 自动放行
│
├─ Step 6: auto_approve 白名单 → 放行
│
├─ Step 7: CLI 自动审批（交互式 + 非嵌套 + 非 DANGEROUS）
│
├─ Step 8: 受信通道（trusted_channels）
│
├─ Step 9: 持久审批记录（allowlist 匹配）
│
├─ Step 10: Smart Approval（LLM 预筛）
│   ├─ approve → 放行
│   ├─ deny → 拒绝
│   └─ escalate/unavailable → 继续
│
└─ Step 11: 人工审批流程
    ├─ 嵌套调用 → 直接拒绝（worker 无法回答审批提示）
    └─ 发送审批请求，等待人工决策
```

---

## ToolExecutionContext

每次工具执行时，框架注入一个冻结的上下文对象 `ToolExecutionContext`，工具据此判断调用来源和权限：

```python
@dataclass(frozen=True)
class ToolExecutionContext:
    execution_id: str       # 本次执行唯一 ID
    trace_id: str           # 整条链路追踪 ID
    session_key: str        # 会话标识（锁/历史/投递）
    memory_scope: str       # 记忆作用域（按人归一）
    user_id: str            # 发起用户
    agent_id: str           # 当前代理
    attempt_index: int      # 重试序号
    idempotency_key: str    # 幂等键（trace+tool+index+params 哈希）
    is_replay: bool         # 是否为重放（回放时跳过副作用）
    parent_execution_id: str | None  # 父执行 ID（嵌套调用）
    credentials: dict       # 凭据注入
    approved_actions: frozenset[str]  # 已审批的动作集合
    approval_source: str    # "human" 或 "auto"
    allowed_tools: frozenset[str]    # 本次允许的工具子集
    channel: str            # 来源通道
    chat_id: str            # 聊天 ID
    reply_to_id: str        # 回复目标消息 ID
    unattended: bool        # 是否无人值守
    cron_authorized: bool   # 是否由已授权的定时任务触发
    inbound_event_id: str   # 入站事件 ID
```

### 关键字段说明

**`approval_source`**：区分工具获批的方式。

- `"human"`：仅当人工在审批提示中明确批准本次调用时才设置
- `"auto"`：所有策略性放行（白名单、受信通道、cli_auto_approve 等）

工具可据此决定是否授予持久性权限。例如 `cronjob` 工具仅在 `approval_source == "human"` 时才签发 `cron_authorized` 授权。

**`unattended`** 与 **`cron_authorized`**：

| 场景 | unattended | cron_authorized | 效果 |
|------|:----------:|:---------------:|------|
| 用户交互式会话 | `False` | `False` | 走标准审批流程 |
| 定时任务触发（已授权） | `True` | `True` | WRITE/EXEC 自动放行 |
| 定时任务触发（未授权） | `True` | `False` | 按 `unattended_policy` |
| 子代理 worker | 继承父级 | 继承父级 | 不可独立提升权限 |

!!! warning "DANGEROUS 工具在无人值守下始终拒绝"
    即使 `cron_authorized=True`，DANGEROUS 级工具仍会被拒绝。这是为了防止无人值守的任务自我提权（如创建新定时任务、安装技能）。

---

## 工具配置

### 工具档位（Profile）

通过 `config.tools.profile` 选择预设工具集：

| 档位 | 包含工具 | 典型场景 |
|------|----------|----------|
| `minimal` | 只读 + clarify + message + notify + todo | 纯问答机器人 |
| `messaging` | minimal + image_gen + memory + tts + vision | 多模态聊天 |
| `coding` | messaging + 文件写入 + knowledge_index + patch + task + workflow | 编码助手 |
| `full` | 所有工具（`*`） | 全功能代理 |

### 允许/拒绝列表

```yaml
tools:
  profile: full
  allow: []          # 覆盖档位，显式允许的工具列表（设置后 profile 失效）
  also_allow: []     # 在档位基础上额外允许
  deny: []           # 显式禁用（优先级最高）
```

`deny` 优先级高于 `allow` 和 `also_allow`。

### 安全策略档位

`config.security.profile` 在工具档位之上叠加安全限制：

| 策略 | 额外拦截 | 适用场景 |
|------|----------|----------|
| `personal_cli` | 无额外限制 | 本地开发/个人使用 |
| `daemon` | 拦截 exec/execute_code/process/skill_install | 后台守护进程 |
| `public_gateway` | 拦截所有写入 + 执行 + 危险工具 | 面向公网的网关 |

### 网络策略

```yaml
execution:
  network_policy: deny   # deny | allow | restricted
```

当设为 `deny` 时，`web_fetch`、`web_search` 以及所有声明 `network.outbound` 能力的工具将被过滤。

### 审批配置

```yaml
permissions:
  approval:
    auto_approve: [exec]          # 免审批放行的工具列表
    trusted_channels: [telegram]  # 受信通道（EXEC 可放行，DANGEROUS 仍需审批）
    cli_auto_approve: true        # CLI 交互模式下 EXEC 自动放行
    mode: "smart"                 # off | smart | strict
    unattended_policy: "deny"     # deny | allow_safe
    wait_timeout_seconds: 120     # 人工审批超时
    smart_model: ""               # Smart Approval 使用的模型
  elevated:
    enabled: true                 # 启用提权机制
    allow_from:                   # 各通道允许提权的用户映射
      telegram: [user_123]
```

`allow_from` 属于 `permissions.elevated`，不在 `approval` 下；`elevated.enabled` 为 false 时该映射不生效。

!!! note "匹配规则"
    `allow_from` 的值按通道的 `sender_id` 做字符串精确匹配。`"*"` 既可作通道键（对所有通道生效）也可作用户值（对该通道所有用户生效）。`permissions.admin_users` 中的用户始终视为已提权。提权只对 `exec`、`execute_code`、`process` 三个工具生效，且仅当执行落在 local/remote 宿主或 `tools.exec.security` 为 `full` 时才需要。

### 工作区限制

```yaml
tools:
  restrict_to_workspace: false   # 是否将文件操作限制在工作区内
  safe_write_root: ""            # 允许写入的根目录（为空则不限）
```

---

## Smart Approval（LLM 预筛）

当审批模式为 `smart` 时，EXEC 级工具调用在请求人工审批前先经过 LLM 预筛：

1. 将工具名、参数、标记原因注入审查提示
2. 要求 LLM 返回 `APPROVE`、`DENY` 或 `ESCALATE`
3. 结果处理：
   - `APPROVE` → 直接放行
   - `DENY` → 直接拒绝
   - `ESCALATE` / 无法识别 → 进入人工审批

---

## 代码示例

### 自定义工具

```python
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult

class MyCustomTool(Tool):
    name = "my_tool"
    description = "执行自定义操作"
    parameters = {
        "type": "object",
        "properties": {
            "input": {"type": "string", "description": "输入内容"},
        },
        "required": ["input"],
    }
    timeout_seconds = 60
    max_retries = 2
    risk_level = "write"
    capabilities = ("fs.write",)

    def is_ready(self) -> bool:
        # 检查外部依赖
        return True

    def readiness_detail(self) -> tuple[bool, str]:
        return True, "所有依赖就绪"

    async def execute(self, params: dict, ctx: ToolExecutionContext | None = None) -> ToolResult:
        input_text = params["input"]
        # ... 执行逻辑 ...
        return ToolResult(success=True, output=f"处理完成: {input_text}")
```

### 利用上下文进行权限检查

```python
async def execute(self, params: dict, ctx: ToolExecutionContext | None = None) -> ToolResult:
    if ctx and ctx.unattended and "my_tool" not in ctx.approved_actions:
        return ToolResult(
            success=False,
            error="无人值守模式下需要显式授权",
            error_kind="business",
        )

    if ctx and ctx.approval_source != "human":
        # 仅人工审批时才执行高危操作
        return ToolResult(
            success=False,
            error="此操作需要人工确认",
            error_kind="business",
        )

    # ... 执行逻辑 ...
    return ToolResult(success=True, output="ok")
```

### 配置工具策略（config.yaml）

```yaml
tools:
  profile: coding
  also_allow:
    - exec
    - process
  deny:
    - skill_install
    - cronjob
  restrict_to_workspace: true
  safe_write_root: /home/user/project

permissions:
  approval:
    mode: smart
    auto_approve:
      - exec
    trusted_channels:
      - cli
    unattended_policy: deny

security:
  profile: daemon

execution:
  network_policy: allow
```

---

## 安全守卫（Guard）

除风险等级外，Shell/Process 类工具还经过模式匹配守卫：

### 硬拦截（Hard Block）

以下模式匹配时直接拒绝，不可通过审批绕过：

- 根目录递归删除（`rm -rf /home`、`rm -rf /etc`）
- 块设备写入（`dd of=/dev/...`）
- 文件系统格式化（`mkfs.*`）
- 系统关机（`shutdown`、`reboot`、`halt`）
- 敏感凭证路径读取（`/etc/shadow`、`/root/.ssh`）

### 软拦截（需审批）

以下模式匹配时需要审批才能继续：

- 递归删除（`rm -r`）
- 全权限设置（`chmod 777`）
- 服务控制（`systemctl stop/restart`）
- 管道执行（`curl ... | bash`）
- 内联解释器（`python -c`、`bash -c`）
- 凭证文件写入（重定向到 `.env`、`id_rsa` 等）
- 破坏性 SQL（`DROP TABLE`、`TRUNCATE`）

!!! warning "SSRF 防护"
    `web_fetch` 工具在目标为内网地址（`127.0.0.1`、`10.*`、`192.168.*` 等）时会触发审批，防止 SSRF 攻击。

---

## 能力声明

每个工具通过 `capabilities` 属性或静态映射 `TOOL_CAPABILITIES` 声明其能力。能力用于：

- 工具策略过滤（`PUBLIC_GATEWAY_DENY_CAPABILITIES`、`DAEMON_DENY_CAPABILITIES`）
- 审计日志分类
- MCP 工具统一标记为 `mcp.call`

完整能力列表：

| 能力 | 含义 |
|------|------|
| `fs.read` | 文件系统读取 |
| `fs.write` | 文件系统写入 |
| `process.exec` | 进程执行 |
| `process.manage` | 进程管理（信号/stdin） |
| `code.exec` | 代码执行 |
| `network.outbound` | 出站网络请求 |
| `scheduler.write` | 定时任务创建 |
| `skill.install` | 技能安装 |
| `skill.write` | 技能管理 |
| `skill.read` | 技能查看 |
| `workflow.write` | 工作流编排 |
| `memory.read` | 记忆读取 |
| `memory.write` | 记忆写入 |
| `message.send` | 消息发送 |
| `message.ask` | 向用户提问 |
| `media.generate` | 媒体生成 |
| `media.read` | 媒体分析 |
| `knowledge.read` | 知识库检索 |
| `knowledge.write` | 知识库索引 |
| `session.read` | 会话检索 |
| `task.write` | 任务管理 |
| `agent.read` | 代理列表 |
| `agent.dispatch` | 代理路由 |
| `mcp.call` | MCP 工具调用 |
