# 术语表

Echo Agent 项目中使用的核心概念与术语定义。

---

## A

### Agent

自主运行的 AI 实体，具备记忆、技能和工具调用能力。Echo Agent 中的 Agent 是长期运行的，与单次对话式 AI 不同。

### A2A（Agent-to-Agent）

Agent 间基于 JSON-RPC 的任务协议。Echo Agent 当前实现入站服务：外部 peer 可向本实例委派文本任务，但本实例尚无生产可用的出站 A2A 委派入口。

### Approval（审批）

当工具的 `approval_mode` 设为 `ask` 时，Agent 在执行该工具前需要获得用户的明确许可。审批通过 TUI 的 `/approve` 和 `/deny` 命令处理。

---

## B

### Bus（事件总线）

Echo Agent 内部的事件分发系统，各组件通过事件总线进行松耦合通信。

---

## C

### Channel（通道）

Agent 与外部世界通信的接口。每个通道对应一个平台集成（如 Slack、Telegram、Discord、WebSocket、CLI）。

### Checkpoint（检查点）

系统状态的完整快照，包含数据库、记忆和配置。用于灾难恢复和状态回滚。

### Circuit Breaker（熔断器）

当某个服务（如 API 端点）连续失败达到阈值时，自动切断请求以防止级联故障。经过恢复等待期后进入半开状态尝试恢复。

### Clarification（澄清）

Agent 在信息不足时向用户提问的机制。通过 `clarify` 工具触发，用户可通过 `/clarify` 命令回复。

### Compression（压缩）

上下文窗口管理策略。当对话历史超过模型上下文限制时，自动压缩早期消息以保留关键信息。

### Cron Job（定时任务）

基于 cron 表达式的计划任务。Agent 按预定时间自动执行指定操作。

---

## D

### Dashboard

Web 管理面板，提供实时系统监控、会话管理、费用查看等功能。通过 `/ws/dashboard` WebSocket 接收实时更新。

### Delegate（委派）

Agent 将子任务分配给另一个 Agent 执行。属于多 Agent 协作模式的核心操作。

---

## E

### Evolution（进化）

技能进化系统。Echo Agent 通过评估现有技能的表现，自动生成改进版本的候选技能。候选需通过评估和人工审批后才能正式上线。

### Execution（执行）

工具调用的实际运行过程，包含超时控制、重试逻辑和结果捕获。

---

## G

### Gateway

Echo Agent 的 HTTP/WebSocket 网关服务。提供 REST API、WebSocket 实时通信以及 Dashboard 静态资源服务。

---

## K

### Knowledge（知识库）

结构化的外部知识存储。支持文档导入、自动分块和向量检索。与 Memory 不同，Knowledge 通常是静态的参考资料。

---

## M

### Memory（记忆）

Agent 的长期记忆系统。分为显式记忆（用户指定保存）和隐式记忆（自动提取的关键信息）。记忆跨会话持久化。

### Migration（迁移）

数据库 Schema 或数据格式的版本升级操作。通过 `echo-agent migrate` 命令管理。

### Multi-Agent（多 Agent）

多个 Agent 实例协作的运行模式。Agent 可以通过 `delegate` 工具将任务分配给其他 Agent。

---

## O

### Observability（可观测性）

系统的可观察能力，包含三个支柱：日志（loguru）、追踪（OpenTelemetry Traces）、指标（OpenTelemetry Metrics / Prometheus）。

---

## P

### Pairing（配对）

Gateway 的一种认证模式。客户端通过短期有效的配对码完成首次认证，适合 TUI 客户端连接场景。

### Permission（权限）

控制 Agent 可以执行的操作范围。通过 Security Profile 和 Tools Profile 两个维度管理。

### Plugin（插件）

可热插拔的功能扩展模块。插件可以添加新的工具、通道或处理逻辑。

### Profile

预定义的配置模板。Echo Agent 有两类 Profile：

- **Security Profile**: minimal / standard / extended（安全级别）
- **Tools Profile**: minimal / messaging / coding / full（工具范围）

---

## R

### Rate Limit（频率限制）

对 API 请求频率的限制，防止资源滥用。支持按分钟请求数和按分钟 Token 数两个维度。

### Risk Category（风险类别）

工具按潜在危害程度的分类。四个类别依次递增：MINIMAL_TOOLS → MESSAGING_TOOLS → CODING_TOOLS → HIGH_RISK_TOOLS。

---

## S

### Sandbox（沙箱）

隔离的执行环境。工具（尤其是 `shell` 和 `code_exec`）在沙箱中运行，限制其对系统资源的访问。

### Session（会话）

用户与 Agent 的一次交互上下文。每个会话有独立的消息历史和状态。会话可通过 ID 恢复。

### Skill（技能）

Agent 学习并可复用的能力单元。技能通过 Evolution 系统自动产生，经过评估和审批后激活。

### Spill（溢出）

当工具输出或上下文内容过大无法直接包含在对话中时，系统将内容写入溢出存储，并在对话中保留引用 ID。通过 `read_spill` 工具按需读取。

### Staged（暂存）

技能的待审批状态。进化系统产生的候选技能或外部安装的技能先进入暂存区，需人工审批后方可使用。

---

## T

### Tool（工具）

Agent 可以调用的功能模块，用于执行特定操作（搜索、文件读写、消息发送等）。Echo Agent 内置 30 个工具。

### TUI（Terminal User Interface）

终端用户界面。基于 Rich/Textual 构建的交互式终端客户端，支持斜杠命令、审批操作和主题切换。

---

## W

### Workspace（工作区）

项目级的数据与配置范围。工作区目录（`.echo-agent/`）位于项目根目录下，包含项目级配置覆盖和本地数据。

### Workflow（工作流）

多步骤的结构化任务流程。通过 `workflow` 工具定义和执行，支持条件分支和并行步骤。

---

## 缩写对照

| 缩写 | 全称 | 中文 |
|------|------|------|
| A2A | Agent-to-Agent | Agent 间协议 |
| API | Application Programming Interface | 应用编程接口 |
| CLI | Command Line Interface | 命令行界面 |
| CORS | Cross-Origin Resource Sharing | 跨域资源共享 |
| DSN | Data Source Name | 数据源名称 |
| LLM | Large Language Model | 大语言模型 |
| OTel | OpenTelemetry | 开放遥测 |
| PID | Process ID | 进程标识 |
| SSE | Server-Sent Events | 服务端推送事件 |
| TLS | Transport Layer Security | 传输层安全 |
| TUI | Terminal User Interface | 终端用户界面 |
| WAL | Write-Ahead Logging | 预写日志 |
| WSL | Windows Subsystem for Linux | Windows Linux 子系统 |
| WS | WebSocket | — |
| YAML | YAML Ain't Markup Language | — |
