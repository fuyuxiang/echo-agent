# 术语表

**适用范围：两端；仅适用于某一产品的定义会明确标注。**

以下定义以当前源码中的配置、路由和类型为准。

| 术语 | 定义 |
|---|---|
| **A2A** | Agent 间基于 JSON-RPC 的任务协议；当前实现入站服务。 |
| **Agent Loop** | 从入站事件到响应的处理循环。 |
| **Approval** | 工具调用前的审批；模式由 `permissions.approval.mode` 控制。 |
| **Channel** | 连接 Telegram、Discord、CLI 等入口的消息适配器。 |
| **Checkpoint** | 工作区文件的影子 Git 快照；不包含数据库、会话、记忆和日志，不能代替完整备份。 |
| **Clarification** | Agent 在信息不足时请求用户补充内容的流程。 |
| **Compression** | 压缩较早会话消息以控制模型上下文长度。 |
| **Cron Job** | 由调度器按计划触发的任务。 |
| **Dashboard** | 使用 Gateway API 和 `/ws/dashboard` 的 Web 管理界面。 |
| **Execution Backend** | 执行类工具使用的后端，由 `tools.exec.host` 选择；`sandbox` 复制工作目录，但不提供操作系统级隔离。 |
| **Gateway** | 提供 HTTP API、WebSocket 和 Dashboard 静态资源的服务。 |
| **Knowledge** | 从文档建立索引供检索的知识库，与记忆存储分开。 |
| **Memory** | 按类型和作用域保存的持久记忆；受检索与遗忘策略影响。 |
| **Migration** | SQLite 表结构迁移在数据库初始化时自动运行；`echo-agent migrate` 只处理 USER 记忆归属和旧 `MEMORY.*.md` 导入。 |
| **Multi-Agent** | 由多个 Agent 协作完成任务的运行方式。 |
| **OpenTelemetry** | 可选的追踪和指标导出框架；当前代码创建追踪 span，但没有文档曾列举的具名业务指标工具。 |
| **Pairing** | Gateway 对平台与用户身份的短期验证码授权流程；不颁发 API token。 |
| **Plugin** | 可加载的 Python 扩展，可注册工具和生命周期钩子；不自动接线独立通道包。 |
| **Risk Category** | 工具风险级别为 `READ_ONLY`、`WRITE`、`EXEC`、`DANGEROUS`；`MINIMAL_TOOLS` 等名称是工具档位集合，不是风险级别。 |
| **Security Profile** | 运行形态档位：`personal_cli`、`daemon`、`public_gateway`。 |
| **Session** | 由通道、用户、聊天和可选线程信息确定的对话上下文。 |
| **Skill** | 以 `SKILL.md` 描述的知识或工作流；部分候选技能需先审批。 |
| **Spill** | 过大的工具输出存储在外部，并在对话中保留引用供 `read_spill` 读取。 |
| **Tool** | Agent 可调用的操作接口；命令执行类工具名包括 `exec`、`execute_code` 和 `process`。 |
| **Tools Profile** | 工具准入集合：`minimal`、`messaging`、`coding`、`full`。 |
| **Workspace** | 配置、运行数据和工作文件所在的目录。 |

## 产品与工作流术语

| 术语 | 定义 |
| --- | --- |
| Desktop / Client | 本地桌面工作台，内嵌 Rust 执行引擎。 |
| 独立 Runtime | Python Agent 服务，可通过终端、通道或 Gateway 使用。 |
| ACP | Agent Client Protocol；桌面端使用类型化进程内通道连接宿主与执行引擎。 |
| 宿主 | 管理界面、资源、本机服务和执行引擎生命周期的组件。 |
| 交付物（Artifact） | 实际生成并可核对内容与保存位置的任务文件。 |
| 专家团 | 桌面端保存成员、工作、消息和决定的协作单元。 |
| 来源依据（Provenance） | 支持事实、记忆或结果的来源和证据。 |
| 验收标准 | 判断任务是否完成的可观察条件。 |
