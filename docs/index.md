# Echo Agent 文档

**Echo Agent** 是一个自托管、长驻运行的 AI Agent 运行时，具备记忆持久化、技能自进化和多通道集成能力。

---

## 快速导航

<div class="grid cards" markdown>

-   :material-rocket-launch:{ .lg .middle } **快速开始**

    ---

    5 分钟完成安装与首次对话

    [:octicons-arrow-right-24: 开始](getting-started/index.md)

-   :material-server-network:{ .lg .middle } **后台运行**

    ---

    以守护进程或 systemd 服务部署，保持 Agent 7×24 在线

    [:octicons-arrow-right-24: 部署指南](operations/deployment.md)

-   :material-chat-processing:{ .lg .middle } **接入平台**

    ---

    将 Agent 接入钉钉、飞书、微信、Slack、Telegram 等 14 个通道

    [:octicons-arrow-right-24: 通道配置](integrations/channels/index.md)

-   :material-puzzle:{ .lg .middle } **扩展开发**

    ---

    编写自定义技能、插件和通道适配器

    [:octicons-arrow-right-24: 开发指南](development/index.md)

</div>

---

## 核心能力

| 模块 | 说明 | 详见 |
|------|------|------|
| **Agent Loop** | 接收事件 → 构建上下文 → 调用模型 → 执行工具，跨入口共享同一条执行路径 | [Agent 循环](concepts/agent-loop.md) |
| **认知记忆** | Working / Episodic / Semantic / Archival 四层，配合衰减、矛盾检测与重要性重排 | [记忆系统](concepts/memory-system.md) |
| **混合检索** | BM25 + FAISS 向量融合召回，按查询特征自适应权重，FAISS 缺失时自动降级 | [知识库](guides/knowledge-base.md) |
| **自进化引擎** | 轨迹记录 → 候选生成 → 评测对照 → 晋升/驳回，支持冷却期与一键回滚 | [技能进化与评测](concepts/evolution-evaluation.md) |
| **模型路由** | 主推理、上下文压缩、向量嵌入、风险审批可独立配置 provider 与模型 | [路由与 Fallback](guides/models/routing-fallback.md) |
| **工具审批** | 三档策略 `manual` / `smart` / `off`，无人值守通道默认拒绝高风险调用 | [工具与权限](guides/tools-permissions.md) |
| **多模型支持** | OpenAI、Anthropic、Gemini、Bedrock、OpenRouter，以及 DeepSeek、Qwen、Kimi、GLM、Ollama 等 OpenAI 兼容端点 | [Provider 总览](guides/models/providers.md) |
| **14 通道适配器** | CLI、Cron、钉钉、Discord、Email、飞书、Matrix、QQ Bot、Slack、Telegram、Webhook、企业微信、微信、WhatsApp | [通道配置](integrations/channels/index.md) |
| **跨进程互操作** | A2A JSON-RPC 入站服务 + MCP 客户端（含 OAuth），支持动态工具注册 | [MCP](integrations/mcp.md) · [A2A](integrations/a2a.md) |
| **插件体系** | 通过 entry-point 注册外部插件 | [使用插件](integrations/plugins/using-plugins.md) |
| **Dashboard** | 内置 Web 管理面板，查看对话、费用与运行状态 | [Dashboard](guides/dashboard.md) |
| **定时任务** | 内置 Cron 调度器，按计划触发 Agent 执行 | [定时任务](guides/scheduled-jobs.md) |
| **输出保全** | 超长工具输出落盘保全，模型只见首尾预览与取回路径，可用 `read_spill` 按字符区间或正则取回完整内容 | [上下文压缩与输出保全](concepts/context-compression-spill.md) |
| **本地优先** | 会话、记忆、轨迹、凭证默认存放工作区，凭证加密落盘 | [安全模型](concepts/security-model.md) |

---

## 项目状态

!!! warning "Beta 阶段"
    Echo Agent 当前版本为 **v0.3.8**，处于 Beta 阶段。核心 API 趋于稳定，但在以下方面仍可能发生破坏性变更：

    - 配置文件格式（`config.yaml` schema）
    - 插件 / 技能 API 接口
    - 数据库 schema（提供 `echo-agent migrate` 迁移命令）

    建议在升级前阅读 [CHANGELOG](https://github.com/fuyuxiang/echo-agent/blob/master/CHANGELOG.md) 并做好数据备份。

---

## 系统要求

- Python 3.11+
- Linux / macOS / Windows（WSL2 推荐）
- 至少一个模型 API Key（OpenAI、Anthropic 等）

---

## 许可证

Echo Agent 采用 [MIT 许可证](https://github.com/fuyuxiang/echo-agent/blob/master/LICENSE) 开源。
