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
| 工具使用 | 无或有限 | 20+ 内置工具 + MCP 协议扩展 |
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
20+ 内置工具，覆盖常见 Agent 任务场景：

- **Shell 执行** — 在沙箱内运行命令，支持超时控制
- **文件系统** — 读写、搜索、补丁操作，可限制在工作区内
- **Web 搜索** — 联网检索信息
- **代码执行** — 安全执行代码片段
- **图片生成** — 调用模型生成图片
- **视觉理解** — 图片分析与描述
- **TTS 语音合成** — 文字转语音
- **定时任务** — 创建 / 管理 Cron Job
- **记忆操作** — 读写长期记忆
- **会话搜索** — 检索历史对话
- **消息发送** — 跨通道主动推送
- **通知** — 向用户发送提醒
- **任务委派** — 子 Agent 任务分发
- **技能调用** — 调用已安装的技能模块
- **MCP 协议** — 通过 Model Context Protocol 接入外部工具服务器

工具执行具备幂等性保护、重试机制、超时控制和审计日志。

### 记忆系统
双层持久记忆，让 Agent 真正"记住"用户和环境：

- **用户记忆** — 偏好、习惯、长期需求，跨会话保持
- **环境记忆** — 项目背景、工具文档、流程规则、领域知识
- **关键词检索** — 多关键词加权评分搜索
- **重要性衰减** — 自动降低过时记忆的权重
- **冲突合并** — 新旧记忆自动去重与合并
- **自动整合** — 达到阈值后自动归纳压缩
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
- **事件驱动** — 响应特定事件自动执行
- **条件触发** — 满足条件时触发
- 最大并发任务数可配置

### 技能系统
可安装、可配置、带版本管理的能力模块：

- 技能以 `SKILL.md` 为入口，支持 Manifest 声明
- 支持全局 / 工作区 / 会话三种作用域
- 内置技能：论文检索（arXiv）、天气查询、内容摘要、开发计划、技能创建器
- 执行后自动经验学习与质量评审

### 权限与审批
- **六级权限模型** — 用户 / 通道 / 工具 / 文件 / 工作区 / 管理员
- **审批工作流** — 高风险操作需人工确认（approve / deny / whitelist / blacklist）
- **凭证管理** — Token 存储、隔离、轮换、审计
- **管理员白名单** — 可配置 admin 用户列表

### Gateway API
- HTTP REST API（消息、健康检查、会话、统计、配对）
- WebSocket 实时通信
- 渐进式消息编辑
- 媒体缓存
- Hook 系统

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
| `channels.cli.enabled` | 启用 CLI 通道 | 否 | `true` |
| `channels.telegram.enabled` | 启用 Telegram | 否 | `false` |
| `channels.telegram.token` | Telegram Bot Token | 条件 | - |
| `tools.exec.enabled` | 启用命令执行工具 | 否 | `true` |
| `tools.exec.timeoutSeconds` | 命令执行超时（秒） | 否 | `30` |
| `tools.web.enabled` | 启用 Web 搜索工具 | 否 | `true` |
| `tools.restrictToWorkspace` | 限制工具在工作区内操作 | 否 | `false` |
| `memory.enabled` | 启用记忆系统 | 否 | `true` |
| `memory.vectorEnabled` | 启用向量检索 | 否 | `false` |
| `memory.consolidationThreshold` | 记忆整合触发阈值 | 否 | `50` |
| `session.maxHistoryMessages` | 最大历史消息数 | 否 | `500` |
| `session.expiryHours` | 会话过期时间（小时） | 否 | `72` |
| `session.contextWindowTokens` | 上下文窗口大小 | 否 | `65536` |
| `compression.enabled` | 启用上下文压缩 | 否 | `true` |
| `compression.triggerRatio` | 压缩触发比例 | 否 | `0.7` |
| `permissions.adminUsers` | 管理员用户列表 | 否 | `[]` |
| `permissions.approval.defaultPolicy` | 默认审批策略 | 否 | `ask` |
| `scheduler.enabled` | 启用调度器 | 否 | `true` |
| `scheduler.maxConcurrentJobs` | 最大并发任务数 | 否 | `10` |
| `gateway.enabled` | 启用 Gateway API | 否 | `false` |
| `gateway.port` | Gateway 端口 | 否 | `9000` |
| `storage.backend` | 存储后端 | 否 | `sqlite` |
| `storage.databasePath` | 数据库路径 | 否 | `data/echo_agent.db` |
| `observability.logLevel` | 日志级别 | 否 | `INFO` |
| `observability.traceEnabled` | 启用追踪 | 否 | `true` |

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

### 权限控制
- 六级权限模型：用户 → 通道 → 工具 → 文件 → 工作区 → 管理员
- 每条权限规则支持优先级排序
- 通道级别 `allow_from` 白名单控制接入

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
│   │   ├── context.py           # 系统提示词 + 上下文组装
│   │   ├── compression/         # 多阶段上下文压缩
│   │   ├── tools/               # 20+ 内置工具 + 注册表
│   │   └── executors/           # 本地 / 远程执行器
│   ├── models/
│   │   ├── provider.py          # LLM 抽象接口
│   │   ├── router.py            # 模型路由 + Fallback + 健康追踪
│   │   ├── providers/           # OpenAI / Anthropic / Bedrock / Gemini / OpenRouter
│   │   ├── inference.py         # 推理管理
│   │   ├── rate_limiter.py      # Token Bucket 限速
│   │   └── credential_pool.py   # 凭证池轮换
│   ├── memory/                  # 双层持久记忆（用户 + 环境）
│   ├── session/                 # JSONL 持久化会话管理
│   ├── skills/                  # 技能安装 / 管理 / 评审
│   ├── bus/                     # 异步发布/订阅事件总线
│   ├── scheduler/               # Cron / 间隔 / 事件 / 条件调度
│   ├── permissions/             # 六级权限 + 审批工作流 + 凭证库
│   ├── gateway/                 # HTTP/WebSocket API 服务
│   ├── mcp/                     # Model Context Protocol 客户端
│   ├── observability/           # 追踪、健康检查、状态检视
│   ├── storage/                 # SQLite 存储后端
│   ├── workspace/               # 工作区管理
│   ├── tasks/                   # 任务规划 + 子 Agent 委派
│   ├── cli/                     # 交互式配置向导
│   └── utils/                   # 文本工具函数
├── skills/                      # 内置技能定义
│   ├── development/             # plan、skill-creator
│   ├── productivity/            # summarize、weather
│   └── research/                # arxiv
├── pyproject.toml               # 构建配置与依赖
└── .github/workflows/ci.yml    # CI：Python 3.11/3.12/3.13 测试
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
- 支持工具白名单 / 黑名单过滤

## Roadmap

- [x] 14 通道接入（Telegram / Discord / Slack / 微信 / QQ / 飞书 / 钉钉 / 企业微信 / WhatsApp / Email / Matrix / Webhook / CLI / Cron）
- [x] 20+ 内置工具
- [x] 双层持久记忆系统
- [x] 多 Provider 模型路由与容错
- [x] 上下文压缩与会话管理
- [x] 定时任务与事件调度
- [x] 技能系统
- [x] 权限与审批工作流
- [x] MCP 协议支持
- [x] Gateway HTTP/WebSocket API
- [ ] Web UI 管理面板
- [ ] 多 Agent 协作编排
- [ ] 更多存储后端（PostgreSQL / Redis）
- [ ] 向量记忆增强检索
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
