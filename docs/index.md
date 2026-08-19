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

    [:octicons-arrow-right-24: 部署指南](deployment/index.md)

-   :material-chat-processing:{ .lg .middle } **接入平台**

    ---

    将 Agent 接入钉钉、飞书、微信、Slack、Telegram 等 14 个通道

    [:octicons-arrow-right-24: 通道配置](channels/index.md)

-   :material-puzzle:{ .lg .middle } **扩展开发**

    ---

    编写自定义技能、插件和通道适配器

    [:octicons-arrow-right-24: 开发指南](development/index.md)

</div>

---

## 核心能力

| 能力 | 说明 |
|------|------|
| **多模型支持** | OpenAI、Anthropic、Gemini、Bedrock、OpenRouter，以及 DeepSeek、Qwen、Kimi 等 OpenAI 兼容端点 |
| **14 通道适配器** | CLI、Cron、钉钉、Discord、Email、飞书、Matrix、QQ Bot、Slack、Telegram、Webhook、企业微信、微信、WhatsApp |
| **长期记忆** | 基于向量检索的对话记忆，跨会话持久化 |
| **技能进化** | Agent 在交互中自动提炼、积累可复用技能 |
| **插件体系** | 通过 entry-point 注册外部插件，支持热加载 |
| **Dashboard** | 内置 Web 管理面板，实时查看对话、费用、状态 |
| **定时任务** | 内置 Cron 调度器，定时触发 Agent 执行任务 |

---

## 项目状态

!!! warning "Beta 阶段"
    Echo Agent 当前版本为 **v0.3.7**，处于 Beta 阶段。核心 API 趋于稳定，但在以下方面仍可能发生破坏性变更：

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
