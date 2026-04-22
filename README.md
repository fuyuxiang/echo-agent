# Echo Agent

> 一个模块化、可私有部署的 AI Agent 框架，支持 14+ 消息通道接入、工具调用、持久记忆与自动化任务编排。

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
[![Python](https://img.shields.io/badge/python-3.11%2B-brightgreen)](#快速开始)
[![CI](https://img.shields.io/badge/CI-passing-brightgreen)](#)

## 项目简介

Echo Agent 是一个面向开发者和团队的开源 AI Agent 框架。它不是一个简单的聊天机器人——而是一个可以长期运行、具备工具调用能力、拥有持久记忆、支持多通道接入的智能代理系统。

**适合谁用：**
- 需要将 AI 助手接入 Telegram / Discord / 飞书 / 企业微信等多个平台的团队
- 希望 Agent 能调用外部工具（搜索、执行代码、操作文件、生成图片）完成复杂任务的开发者
- 需要私有化部署、数据不出域的企业场景

**和传统 Chatbot 的区别：**

| | 传统 Chatbot | Echo Agent |
|---|---|---|
| 对话能力 | 单轮/短期多轮 | 长期会话 + 上下文压缩 |
| 工具使用 | 无或有限 | 26+ 内置工具 + MCP 协议扩展 |
| 记忆 | 无 | 双层持久记忆（用户记忆 + 环境记忆） |
| 通道 | 单一平台 | 14 个通道统一接入 |
| 自动化 | 无 | 定时任务 / 事件驱动 / 条件触发 |
| 部署 | SaaS 依赖 | 完全自托管 |

## 架构总览

```text
┌─────────────────────────────────────────────────────────┐
│                      输入层 (Channels)                    │
│  Telegram · Discord · Slack · 微信 · QQ · 飞书 · 钉钉     │
│  企业微信 · WhatsApp · Email · Matrix · Webhook · CLI      │
└──────────────────────┬──────────────────────────────────┘
                       │ Event Bus (异步发布/订阅)
                       ▼
┌─────────────────────────────────────────────────────────┐
│                   Agent Runtime 核心                      │
│  ┌───────────┐ ┌───────────┐ ┌────────────┐             │
│  │ 会话管理   │ │ 上下文压缩 │ │ 权限与审批  │             │
│  └───────────┘ └───────────┘ └────────────┘             │
│  ┌───────────┐ ┌───────────┐ ┌────────────┐             │
│  │ 工具注册表 │ │ 记忆系统   │ │ 技能系统   │             │
│  └───────────┘ └───────────┘ └────────────┘             │
│  ┌───────────┐ ┌───────────┐ ┌────────────┐             │
│  │ 任务规划   │ │ 子Agent   │ │ 上下文构建  │             │
│  └───────────┘ └───────────┘ └────────────┘             │
└──────────────────────┬──────────────────────────────────┘
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
┌──────────────┐ ┌──────────┐ ┌──────────────┐
│   模型层      │ │  工具层   │ │   存储层      │
│ OpenAI       │ │ Shell    │ │ SQLite       │
│ Anthropic    │ │ Web 搜索  │ │ JSONL 会话   │
│ Bedrock      │ │ 文件系统  │ │ JSON 记忆    │
│ Gemini       │ │ 代码执行  │ │ FAISS 向量   │
│ OpenRouter   │ │ 图片生成  │ │ (可选)       │
│ 兼容 API     │ │ MCP 扩展  │ │              │
└──────────────┘ └──────────┘ └──────────────┘
          ▲                          ▲
          │                          │
┌──────────────┐            ┌──────────────┐
│  执行环境     │            │  Gateway API │
│ Local/Docker │            │ HTTP / WS    │
│ Sandbox/远程  │            │ 认证 / 限速   │
└──────────────┘            └──────────────┘
```

## 核心特性

### 多通道接入
14 个消息通道开箱即用，统一消息模型，一套逻辑服务所有平台：

| 通道 | 协议 | 说明 |
|---|---|---|
| CLI | stdin/stdout | 本地调试与开发 |
| Telegram | Bot API 长轮询 | 支持代理、群组 @提及策略 |
| Discord | WebSocket Gateway v10 | 心跳保活、断线重连 |
| Slack | Socket Mode | WebSocket + Web API |
| 微信公众号 | 官方 API | XML Webhook、Access Token 自动轮换 |
| QQ 机器人 | 官方 API v2 | WebSocket 网关、沙箱模式 |
| 飞书 / Lark | 事件订阅 v2 | AES 解密、Webhook 回调 |
| 钉钉 | 机器人 Webhook | 消息推送 |
| 企业微信 | 企业 API | Webhook 回调 |
| WhatsApp | Meta Cloud API v21.0 | Webhook + REST |
| Email | IMAP + SMTP | SSL 支持、轮询收件 |
| Matrix | Homeserver API | 去中心化通信 |
| Webhook | 通用 HTTP | 自定义集成 |
| Cron | 定时注入 | 定时触发消息 |

每个通道支持 `allow_from` 白名单做接入控制，内置 Groq Whisper 语音转文字。

### 工具调用
26 个内置工具，覆盖常见 Agent 任务场景：

- **Shell 执行** — 在沙箱内运行命令，支持超时控制、命令白名单/黑名单
- **文件读取** — 读取文件内容，可限制在工作区内
- **文件写入** — 创建或覆盖文件
- **文件编辑** — 精确编辑文件指定内容
- **目录浏览** — 列出目录结构
- **文件搜索** — 正则 + Glob 模式搜索工作区文件
- **补丁应用** — 统一 diff 或搜索替换，支持模糊匹配
- **Web 搜索** — 联网检索信息
- **Web 抓取** — 获取指定 URL 页面内容
- **代码执行** — 安全执行 Python / JavaScript / Bash 代码片段
- **进程管理** — 后台进程的启动、列表、轮询、停止
- **图片生成** — 调用 DALL-E 等模型生成图片
- **视觉理解** — 图片分析与描述
- **TTS 语音合成** — 文字转语音（Edge TTS / OpenAI TTS）
- **定时任务** — 创建 / 管理 Cron Job
- **记忆操作** — 读写、搜索、更新长期记忆
- **会话搜索** — 检索历史对话
- **消息发送** — 跨通道主动推送消息
- **通知** — 向用户发送提醒
- **澄清提问** — 向用户提出澄清问题（支持多选项）
- **任务委派** — 子 Agent 同步任务分发
- **后台任务** — 异步 fire-and-forget 后台执行
- **待办管理** — 每会话任务规划与追踪（创建/更新/完成/删除）
- **技能列表** — 查看已安装技能
- **技能详情** — 加载技能完整内容
- **技能管理** — 安装、启用、禁用技能
- **MCP 协议** — 通过 Model Context Protocol 接入外部工具服务器

工具执行具备幂等性保护、重试机制、超时控制和审计日志。

### 记忆系统
双层持久记忆，让 Agent 真正"记住"用户和环境：

- **用户记忆** — 偏好、习惯、长期需求，跨会话保持
- **环境记忆** — 项目背景、工具文档、流程规则、领域知识
- **关键词检索** — 多关键词加权评分搜索
- **时间范围检索** — 按时间段查询记忆
- **精确查找** — 按 Key 或内容精确匹配
- **重要性衰减** — 自动降低过时记忆的权重（可配置衰减周期）
- **冲突合并** — 新旧记忆自动去重与合并
- **LLM 驱动整合** — 达到阈值后由 LLM 自动归纳压缩，生成 MEMORY.md 长期记忆文件和 HISTORY.md 历史摘要
- **自动提取** — Memory Reviewer 从对话中自动提取值得记住的信息
- **快照注入** — 将记忆快照注入系统提示词，为推理提供上下文
- **向量检索** — 可选启用 FAISS 向量索引（需安装 `faiss-cpu`）

### 会话与上下文管理
- JSONL 持久化会话，重启不丢失
- 多阶段上下文压缩（摘要 + 工具输出裁剪 + 尾部预算控制）
- 可配置上下文窗口大小、历史消息上限、会话过期时间

### 模型支持
6 个 LLM Provider，灵活路由与容错：

| Provider | 默认模型 | 特性 |
|---|---|---|
| OpenAI | gpt-4o | 标准 Chat Completions |
| Anthropic | claude-sonnet-4 | Prompt Caching、自适应思考 |
| AWS Bedrock | Claude via Bedrock | Converse API、AWS 凭证解析 |
| Google Gemini | gemini-2.0-flash | Google GenAI SDK |
| OpenRouter | claude-sonnet-4 | 多 Provider 路由偏好 |
| 兼容 API | 可配置 | 任何 OpenAI 兼容端点 |

跨 Provider 特性：
- 自动重试与指数退避（429 / 5xx / 超时）
- 模型路由 + Fallback 链 + 健康状态追踪
- 凭证池轮换
- Token Bucket 限速
- 每日成本上限

### 自动化与调度
- **Cron 定时任务** — 标准 cron 表达式
- **间隔触发** — 固定间隔执行
- **一次性延迟** — 指定时间点单次执行（ONCE 触发器）
- **事件驱动** — 响应特定事件自动执行
- **条件触发** — 满足条件时触发
- **任务生命周期** — 暂停 / 恢复 / 取消
- **持久化恢复** — 重启后自动恢复未完成的调度任务
- **执行后自删** — 支持 `delete_after_run` 一次性任务
- 最大并发任务数可配置

### 技能系统
可安装、可配置、带版本管理的能力模块：

- 技能以 `SKILL.md` 为入口，支持 Manifest 声明
- 支持全局 / 工作区 / 会话三种作用域
- 内置技能：论文检索（arXiv）、天气查询、内容摘要、开发计划、技能创建器
- 版本管理：支持升级与回滚，保留历史版本记录
- 依赖检查：启用技能时自动校验依赖是否满足
- 经验学习：ExperienceStore 记录执行成功/失败模式，追踪复用次数，匹配相似历史经验
- 渐进式加载：Tier 0 列表 → Tier 1 详情 → Tier 2 完整内容，按需加载
- 多来源发现：用户目录 + 内置目录 + 外部扩展目录

### 权限与审批
- **六级权限模型** — 用户 / 通道 / 工具 / 文件 / 工作区 / 管理员
- **审批工作流** — 高风险操作需人工确认（approve / deny / whitelist / blacklist）
- **凭证管理** — Token 存储、隔离、轮换、审计
- **管理员白名单** — 可配置 admin 用户列表

### Gateway API
- HTTP REST API（消息、健康检查、会话、统计、配对）
- WebSocket 实时通信
- 多种认证模式（allowlist / pairing）
- 按平台限速（Rate Limiting）
- 会话重置策略（idle 超时 / 每日重置）
- 跨平台消息路由（Delivery Router）
- 渐进式消息编辑（流式输出）
- 媒体缓存（可配置大小上限）
- Hook 系统（自定义事件钩子）

### 任务规划与子 Agent
- **目标分解** — LLM 驱动的任务拆解，自动生成子任务与依赖关系
- **状态机** — 完整任务生命周期：pending → queued → running → success / failed / retrying / cancelled / suspended
- **子 Agent 管理** — 独立上下文的并行子 Agent 执行（可配置最大并发数）
- **工作区隔离** — 每个任务可获得独立工作目录，支持变更追踪与回滚

### 执行环境
支持多种命令执行环境，按安全需求选择：

| 执行器 | 说明 |
|---|---|
| `local` | 本地直接执行（默认） |
| `sandbox` | 沙箱隔离执行 |
| `container` | Docker 容器内执行，支持网络策略控制 |
| `remote` | 远程主机执行 |

网络策略：`allow`（允许所有）/ `deny`（禁止网络）/ `restricted`（受限访问）

### 上下文构建
- **Bootstrap 文件** — 支持 `AGENTS.md`、`SOUL.md`、`USER.md`、`TOOLS.md` 自定义 Agent 人设与行为
- **分层注入** — 系统提示词 → 记忆快照 → 技能上下文 → 运行时元数据 → 对话历史 → 检索增强
- **记忆引导** — 自动将记忆操作指南和快照注入上下文
- **技能引导** — 自动将可用技能列表注入上下文

### 可观测性
- 结构化日志（Loguru）
- 分布式追踪
- 健康检查
- Agent 状态检视

## 快速开始

### 环境要求
- Python 3.11+
- 至少一个 LLM Provider 的 API Key（OpenAI / Anthropic / Gemini 等）

### 安装

```bash
git clone https://github.com/yourname/echo-agent.git
cd echo-agent
pip install -e ".[all]"
```

按需安装 Provider：

```bash
# 仅 OpenAI
pip install -e ".[openai]"

# 仅 Anthropic
pip install -e ".[anthropic]"

# 所有 Provider
pip install -e ".[allproviders]"

# 向量检索支持
pip install -e ".[vector]"
```

### 配置

**方式一：交互式向导（推荐首次使用）**

```bash
echo-agent setup
```

向导会引导你配置模型 Provider 和消息通道，自动生成 `echo-agent.yaml`。

**方式二：手动创建配置文件**

在项目根目录创建 `echo-agent.yaml`：

```yaml
models:
  defaultModel: "gpt-4o"
  providers:
    - name: openai
      type: openai
      apiKey: "sk-..."
      models: ["gpt-4o", "gpt-4o-mini"]

channels:
  cli:
    enabled: true
  telegram:
    enabled: true
    token: "your-bot-token"
```

也可通过环境变量覆盖配置，前缀为 `ECHO_AGENT_`。

### 启动

```bash
# CLI 模式（本地对话）
echo-agent

# 指定配置文件
echo-agent -c /path/to/echo-agent.yaml

# 指定工作区目录
echo-agent -w /path/to/workspace
```

### 验证

启动后在 CLI 中输入：

```
> 你好，介绍一下你自己
> 帮我搜索一下今天的科技新闻
> 记住我喜欢用 Python 写代码
```

如果配置了 Telegram 等通道，直接在对应平台给 Bot 发消息即可。

## 配置说明

所有配置项均可在 `echo-agent.yaml` 中设置，也可通过 `ECHO_AGENT_` 前缀的环境变量覆盖。

| 配置项 | 说明 | 必填 | 默认值 |
|---|---|:---:|---|
| `models.defaultModel` | 默认模型名称 | 是 | `gpt-4o` |
| `models.providers[].type` | Provider 类型 | 是 | - |
| `models.providers[].apiKey` | API 密钥 | 是 | - |
| `models.providers[].credentialPool` | 凭证池（多 Key 轮换） | 否 | `[]` |
| `models.providers[].rateLimitRpm` | 每分钟请求限制 | 否 | `0` |
| `models.costLimitDailyUsd` | 每日成本上限（美元） | 否 | `0`（不限） |
| `models.fallbackModel` | 全局 Fallback 模型 | 否 | - |
| `models.routes[].fallbackModels` | 路由级 Fallback 链 | 否 | `[]` |
| `channels.cli.enabled` | 启用 CLI 通道 | 否 | `true` |
| `channels.telegram.enabled` | 启用 Telegram | 否 | `false` |
| `channels.telegram.token` | Telegram Bot Token | 条件 | - |
| `channels.telegram.proxy` | Telegram 代理地址 | 否 | - |
| `channels.sendProgress` | 发送处理进度提示 | 否 | `false` |
| `channels.sendToolHints` | 发送工具调用提示 | 否 | `false` |
| `channels.transcriptionApiKey` | Groq Whisper 语音转文字 Key | 否 | - |
| `tools.exec.enabled` | 启用命令执行工具 | 否 | `true` |
| `tools.exec.timeoutSeconds` | 命令执行超时（秒） | 否 | `30` |
| `tools.exec.maxOutputChars` | 命令输出最大字符数 | 否 | `16000` |
| `tools.exec.allowedCommands` | 命令白名单 | 否 | `[]` |
| `tools.exec.blockedCommands` | 命令黑名单 | 否 | `[]` |
| `tools.web.enabled` | 启用 Web 搜索工具 | 否 | `true` |
| `tools.web.proxy` | Web 请求代理 | 否 | - |
| `tools.codeExec.allowedLanguages` | 允许执行的语言 | 否 | `[python, javascript, bash]` |
| `tools.restrictToWorkspace` | 限制工具在工作区内操作 | 否 | `false` |
| `tools.imageGen.apiKey` | 图片生成 API Key | 否 | - |
| `tools.imageGen.model` | 图片生成模型 | 否 | `dall-e-3` |
| `tools.tts.defaultBackend` | TTS 后端 | 否 | `edge` |
| `tools.mcpServers` | MCP 服务器配置（详见下方） | 否 | `{}` |
| `execution.defaultExecutor` | 执行环境 | 否 | `local` |
| `execution.containerImage` | Docker 镜像 | 否 | - |
| `execution.networkPolicy` | 网络策略 | 否 | `allow` |
| `memory.enabled` | 启用记忆系统 | 否 | `true` |
| `memory.vectorEnabled` | 启用向量检索 | 否 | `false` |
| `memory.consolidationThreshold` | 记忆整合触发阈值 | 否 | `50` |
| `memory.importanceDecayDays` | 重要性衰减周期（天） | 否 | `30` |
| `memory.maxUserMemories` | 用户记忆上限 | 否 | `1000` |
| `memory.maxEnvMemories` | 环境记忆上限 | 否 | `500` |
| `session.maxHistoryMessages` | 最大历史消息数 | 否 | `500` |
| `session.expiryHours` | 会话过期时间（小时） | 否 | `72` |
| `session.archiveAfterHours` | 会话归档时间（小时） | 否 | `168` |
| `session.contextWindowTokens` | 上下文窗口大小 | 否 | `65536` |
| `compression.enabled` | 启用上下文压缩 | 否 | `true` |
| `compression.triggerRatio` | 压缩触发比例 | 否 | `0.7` |
| `permissions.adminUsers` | 管理员用户列表 | 否 | `[]` |
| `permissions.approval.defaultPolicy` | 默认审批策略 | 否 | `ask` |
| `permissions.approval.requireApproval` | 强制审批的操作列表 | 否 | `[]` |
| `permissions.approval.autoApprove` | 自动通过的操作列表 | 否 | `[]` |
| `scheduler.enabled` | 启用调度器 | 否 | `true` |
| `scheduler.maxConcurrentJobs` | 最大并发任务数 | 否 | `10` |
| `gateway.enabled` | 启用 Gateway API | 否 | `false` |
| `gateway.port` | Gateway 端口 | 否 | `9000` |
| `gateway.auth.mode` | 认证模式 | 否 | `allowlist` |
| `gateway.sessionPolicy.mode` | 会话重置策略 | 否 | `idle` |
| `storage.backend` | 存储后端 | 否 | `sqlite` |
| `storage.databasePath` | 数据库路径 | 否 | `data/echo_agent.db` |
| `observability.logLevel` | 日志级别 | 否 | `INFO` |
| `observability.traceEnabled` | 启用追踪 | 否 | `true` |
| `observability.showToolCalls` | 显示工具调用详情 | 否 | `true` |

## 使用示例

### 1. 对话型任务
```
> 帮我解释一下 Python 的 asyncio 事件循环是怎么工作的
> 我上次跟你说过我喜欢什么编程语言来着？
> 把我们之前讨论的方案总结一下
```
Agent 会结合长期记忆和会话上下文给出个性化回答。

### 2. 工具型任务
```
> 搜索一下 "transformer attention mechanism" 的最新论文
> 帮我读一下 /path/to/config.yaml 的内容
> 执行 python scripts/analyze.py 看看输出结果
> 生成一张赛博朋克风格的城市夜景图
```
Agent 会自动选择合适的工具完成任务，并将结果返回。

### 3. 自动化任务
```
> 每天早上 9 点帮我总结 GitHub 上的新 issue
> 每隔 30 分钟检查一下服务器状态
> 当收到包含"紧急"的消息时，转发到我的 Telegram
```
通过调度器配置 Cron / 间隔 / 事件 / 条件触发的自动化工作流。

### 4. 跨通道协作
在 Telegram 中发送指令，Agent 处理后可以通过 Email 发送报告，或在 Slack 中通知团队成员。所有通道共享同一个 Agent 运行时和记忆系统。

## 安全与权限

Echo Agent 具备工具调用和外部系统访问能力，部署前请务必了解以下安全机制：

### 默认安全策略
- 审批策略默认为 `ask` — 高风险操作需人工确认
- 工具执行有超时限制（默认 30 秒）
- 可通过 `tools.restrictToWorkspace: true` 限制文件操作范围
- Shell 工具支持命令白名单 / 黑名单（`allowed_commands` / `blocked_commands`）
- 代码执行限制允许的语言（默认：Python / JavaScript / Bash）

### 权限控制
- 六级权限模型：用户 → 通道 → 工具 → 文件 → 工作区 → 管理员
- 每条权限规则支持优先级排序
- 通道级别 `allow_from` 白名单控制接入
- 审批工作流支持：`require_approval`（强制审批）/ `auto_approve`（自动通过）/ `auto_deny`（自动拒绝）

### MCP 安全
- **Prompt 注入扫描** — 自动检测 MCP 工具描述中的注入攻击模式（指令覆盖、角色注入、越狱尝试、数据外泄等 8 类威胁）
- **工具名冲突检测** — MCP 外部工具不会覆盖内置工具
- **白名单/黑名单** — 可配置 `tools_include` / `tools_exclude` 过滤 MCP 工具

### 执行环境隔离
- Docker 容器执行器支持网络策略（`deny` = 禁止网络访问）
- 沙箱执行器提供文件系统隔离
- 每个任务可获得独立工作目录

### 凭证管理
- API Key 通过配置文件或环境变量注入，不要硬编码
- 凭证池支持轮换，单个 Key 失效自动切换
- 凭证存储支持隔离和审计

### 部署建议
- 生产环境建议启用 `tools.restrictToWorkspace: true`
- 关闭不需要的工具（`tools.exec.enabled: false`）
- 配置 `permissions.adminUsers` 限制管理操作
- 使用环境变量管理敏感信息，不要提交到代码仓库
- 定期审查日志和追踪记录

## 项目结构

```
echo-agent/
├── echo_agent/
│   ├── __main__.py              # 入口，CLI 参数解析，启动引导
│   ├── config/                  # YAML 配置加载 + Pydantic Schema
│   ├── channels/                # 14 个通道适配器 + 通道管理器
│   │   ├── base.py              # 通道基类（含语音转文字）
│   │   ├── manager.py           # 通道生命周期管理
│   │   ├── telegram.py          # Telegram Bot API
│   │   ├── discord.py           # Discord WebSocket Gateway
│   │   ├── slack.py             # Slack Socket Mode
│   │   ├── wechat.py            # 微信公众号
│   │   ├── qqbot.py             # QQ 机器人
│   │   ├── feishu.py            # 飞书 / Lark
│   │   ├── dingtalk.py          # 钉钉
│   │   ├── wecom.py             # 企业微信
│   │   ├── whatsapp.py          # WhatsApp
│   │   ├── email.py             # Email (IMAP/SMTP)
│   │   ├── matrix.py            # Matrix
│   │   ├── webhook.py           # 通用 Webhook
│   │   └── cron.py              # 定时消息注入
│   ├── agent/
│   │   ├── loop.py              # Agent 核心处理循环
│   │   ├── context.py           # 系统提示词 + 分层上下文组装
│   │   ├── compression/         # 多阶段上下文压缩
│   │   ├── tools/               # 26 个内置工具 + 注册表
│   │   └── executors/           # 本地 / 沙箱 / Docker / 远程执行器
│   ├── models/
│   │   ├── provider.py          # LLM 抽象接口
│   │   ├── router.py            # 模型路由 + Fallback + 健康追踪
│   │   ├── providers/           # OpenAI / Anthropic / Bedrock / Gemini / OpenRouter
│   │   ├── inference.py         # 推理管理
│   │   ├── rate_limiter.py      # Token Bucket 限速
│   │   └── credential_pool.py   # 凭证池轮换
│   ├── memory/                  # 双层持久记忆 + LLM 整合 + 自动提取
│   │   ├── store.py             # 记忆存储（CRUD / 检索 / 快照）
│   │   ├── consolidator.py      # LLM 驱动记忆整合 → MEMORY.md / HISTORY.md
│   │   └── reviewer.py          # 对话记忆自动提取
│   ├── session/                 # JSONL 持久化会话管理
│   ├── skills/                  # 技能安装 / 管理 / 评审 / 经验学习
│   │   ├── store.py             # 技能存储（渐进式加载 / 多来源发现）
│   │   └── manager.py           # 技能生命周期（升级 / 回滚 / 依赖检查）
│   ├── tasks/                   # 任务规划 + 状态机 + 子 Agent 委派
│   │   ├── models.py            # 任务数据模型与状态机
│   │   ├── planner.py           # LLM 驱动目标分解
│   │   ├── manager.py           # 任务生命周期管理
│   │   └── subagent.py          # 并行子 Agent 执行
│   ├── bus/                     # 异步发布/订阅事件总线
│   ├── scheduler/               # 5 种触发器调度（Cron/间隔/一次性/事件/条件）
│   ├── permissions/             # 六级权限 + 审批工作流 + 凭证库
│   ├── gateway/                 # HTTP/WebSocket API 服务
│   │   ├── server.py            # 主服务器
│   │   ├── auth.py              # 认证（allowlist / pairing）
│   │   ├── rate_limiter.py      # 按平台限速
│   │   ├── router.py            # 跨平台消息路由
│   │   ├── editor.py            # 渐进式消息编辑
│   │   ├── session_policy.py    # 会话重置策略
│   │   ├── media.py             # 媒体缓存
│   │   ├── hooks.py             # 事件钩子
│   │   └── health.py            # 健康检查
│   ├── mcp/                     # Model Context Protocol 客户端
│   │   ├── manager.py           # MCP 服务器编排
│   │   ├── client.py            # MCP 客户端
│   │   ├── transport.py         # stdio / HTTP 传输
│   │   ├── tool_adapter.py      # MCP 工具适配器
│   │   └── security.py          # 注入扫描 + 冲突检测
│   ├── observability/           # 追踪、健康检查、状态检视
│   ├── storage/                 # SQLite 存储后端
│   ├── workspace/               # 工作区管理（隔离 / 变更追踪 / 上传映射）
│   ├── cli/                     # 交互式配置向导
│   └── utils/                   # 文本工具函数
├── skills/                      # 内置技能定义
│   ├── development/             # plan、skill-creator
│   ├── productivity/            # summarize、weather
│   └── research/                # arxiv
├── pyproject.toml               # 构建配置与依赖
└── .github/workflows/ci.yml    # CI：Python 3.11/3.12/3.13 测试
```
```

## 扩展开发

### 新增一个工具

1. 在 `echo_agent/agent/tools/` 下创建新文件，继承 `Tool` 基类
2. 实现 `name`、`description`、`parameters`（JSON Schema）和 `execute` 方法
3. 在 `__init__.py` 中注册到 `ToolRegistry`

```python
from echo_agent.agent.tools.base import Tool, ToolResult

class MyTool(Tool):
    name = "my_tool"
    description = "做一些有用的事情"
    parameters = {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "查询内容"}
        },
        "required": ["query"]
    }

    async def execute(self, params, ctx=None):
        result = do_something(params["query"])
        return ToolResult(success=True, data=result)
```

### 新增一个通道

1. 在 `echo_agent/channels/` 下创建新文件，继承 `BaseChannel`
2. 实现 `start()`、`stop()` 生命周期方法和消息收发逻辑
3. 收到消息后发布 `InboundEvent` 到 Event Bus
4. 订阅 `OutboundEvent` 将 Agent 回复发送到平台
5. 在 `ChannelManager` 中注册新通道类型

### 新增一个 LLM Provider

1. 在 `echo_agent/models/providers/` 下创建新文件，继承 `LLMProvider`
2. 实现 `chat()` 方法（接收 messages + tools，返回 `LLMResponse`）
3. 实现 `get_default_model()` 方法
4. 在 `providers/__init__.py` 的 `create_provider()` 中注册类型映射

### 新增一个技能

1. 在 `skills/` 下创建目录结构：`category/skill-name/SKILL.md`
2. 编写 `SKILL.md` 作为技能入口（Prompt 模板）
3. 可选：添加 `manifest.json` 声明版本、依赖、配置 Schema
4. 技能支持三种作用域：`global`（全局）、`workspace`（工作区）、`session`（会话）

### 通过 MCP 接入外部工具

Echo Agent 支持 [Model Context Protocol](https://modelcontextprotocol.io)，可以连接外部 MCP 工具服务器：

- 支持 stdio 和 HTTP 两种传输方式
- 自动发现并注册外部工具
- 支持 OAuth 认证
- 支持工具白名单 / 黑名单过滤（`tools_include` / `tools_exclude`）
- 内置安全扫描：自动检测工具描述中的 Prompt 注入攻击
- 工具名冲突保护：外部工具不会覆盖内置工具
- 断线自动重连（指数退避）

配置示例：

```yaml
tools:
  mcpServers:
    my-server:
      command: "npx"
      args: ["-y", "@my/mcp-server"]
      enabled: true
      toolsInclude: ["search", "fetch"]
    remote-server:
      url: "https://mcp.example.com"
      auth: "Bearer xxx"
      enabled: true
```

### 自定义 Agent 人设

在工作区根目录放置以下 Bootstrap 文件，自定义 Agent 的身份和行为：

| 文件 | 用途 |
|---|---|
| `AGENTS.md` | Agent 身份定义、角色描述 |
| `SOUL.md` | Agent 性格、沟通风格、价值观 |
| `USER.md` | 用户画像、偏好、背景信息 |
| `TOOLS.md` | 工具使用指南、自定义工具说明 |

这些文件会在上下文构建时自动注入系统提示词。

## Roadmap

- [x] 14 通道接入（Telegram / Discord / Slack / 微信 / QQ / 飞书 / 钉钉 / 企业微信 / WhatsApp / Email / Matrix / Webhook / CLI / Cron）
- [x] 26 个内置工具
- [x] 双层持久记忆系统 + LLM 驱动整合
- [x] 多 Provider 模型路由与容错
- [x] 上下文压缩与会话管理
- [x] 5 种调度触发器（Cron / 间隔 / 一次性 / 事件 / 条件）
- [x] 技能系统（版本管理 / 经验学习 / 渐进式加载）
- [x] 权限与审批工作流
- [x] MCP 协议支持（含安全扫描）
- [x] Gateway HTTP/WebSocket API
- [x] 任务规划与子 Agent 委派
- [x] 多执行环境（本地 / 沙箱 / Docker / 远程）
- [x] 工作区隔离与变更追踪
- [x] Bootstrap 文件自定义 Agent 人设（AGENTS.md / SOUL.md / USER.md / TOOLS.md）
- [ ] 多 Agent 协作编排（Orchestration，配置已就绪）
- [ ] Web UI 管理面板
- [ ] 更多存储后端（PostgreSQL / Redis）
- [ ] Docker 一键部署镜像
- [ ] 插件市场

## Contributing

欢迎提交 Issue 和 Pull Request。

1. Fork 本仓库
2. 创建特性分支：`git checkout -b feature/my-feature`
3. 提交更改：`git commit -m "feat: add my feature"`
4. 推送分支：`git push origin feature/my-feature`
5. 提交 Pull Request

开发环境：

```bash
pip install -e ".[dev]"
pytest
ruff check .
```

CI 会在 Python 3.11 / 3.12 / 3.13 上运行测试。

## License

MIT
