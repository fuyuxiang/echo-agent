# 仓库地图

Echo Agent 采用模块化单仓架构，后端为 Python 包 `echo_agent/`，前端为独立 SPA `web/`。

## 顶层结构

```
echo-agent/
├── echo_agent/          # Python 主包
├── web/                 # Dashboard 前端（React + Vite）
├── skills/              # 内置 Skill 集合
├── scripts/             # 安装/发布脚本
├── tests/               # pytest 测试套件
├── docs/                # MkDocs 文档源
├── pyproject.toml       # 构建配置、依赖、工具配置
└── .github/workflows/   # CI（lint、test、security、dashboard、docs、package）
```

## 核心子系统

### Agent 核心 — `echo_agent/agent/`

Agent 主循环、工具执行、规划、多 Agent 协作。

Tool 扩展契约的稳定公开入口是 `echo_agent.tools`；`echo_agent/tools/base.py`
是其实现模块，Agent 目录下的 `tools/base.py` 仅保留为旧导入路径的兼容 shim。

```
agent/
├── loop.py              # AgentLoop — 核心推理-执行循环
├── planning/            # 任务规划与分解
├── multi_agent/         # 多 Agent 协作（delegate/spawn）
├── tools/               # 工具实现（shell、filesystem、search 等）
│   ├── base.py          # 旧导入路径的向后兼容 shim
│   ├── registry.py      # ToolRegistry — 注册、权限检查、审计
│   ├── shell.py         # ShellTool (exec) — 命令执行
│   ├── filesystem.py    # 文件读写
│   ├── search.py        # 搜索工具
│   ├── memory.py        # 记忆操作工具
│   ├── knowledge.py     # 知识库查询工具
│   ├── skill_run.py     # Skill 调用
│   ├── delegate.py      # 多 Agent 委派
│   └── ...              # 30+ 工具实现
├── executors/           # 执行器抽象（进程、容器）
└── proc_lifecycle.py    # 子进程生命周期管理
```

### 模型层 — `echo_agent/models/`

多 Provider 抽象、路由、速率控制、凭证池。

```
models/
├── provider.py          # LLMProvider 抽象基类、LLMResponse、ToolCallRequest
├── providers/
│   ├── __init__.py      # Provider 工厂 + _PROVIDER_MAP 注册表
│   ├── openai_provider.py
│   ├── anthropic_provider.py
│   ├── bedrock_provider.py
│   ├── gemini_provider.py
│   └── openrouter_provider.py
├── router.py            # 模型路由（任务→Provider 映射）
├── rate_limiter.py      # 令牌桶限流
└── credential_pool.py   # 多 Key 轮转
```

### 通道层 — `echo_agent/channels/`

14 个消息通道适配器 + 管理器。

```
channels/
├── base.py              # BaseChannel 抽象基类
├── manager.py           # ChannelManager — 启停、路由、投递
├── cli.py               # CLI 通道
├── telegram.py          # Telegram Bot
├── discord.py           # Discord Bot
├── slack.py             # Slack App
├── weixin.py            # 微信公众号/企业微信
├── wecom.py             # 企业微信自建应用
├── feishu.py            # 飞书
├── dingtalk.py          # 钉钉
├── email.py             # 邮件通道
├── webhook.py           # 通用 Webhook
├── cron.py              # 定时触发
├── matrix.py            # Matrix 协议
├── qqbot.py             # QQ Bot
└── whatsapp.py          # WhatsApp Business
```

### 记忆系统 — `echo_agent/memory/`

四层记忆架构：工作记忆、短期、长期、归档。

```
memory/
├── manager.py           # MemoryManager — 统一接口
├── tiers/               # 四层存储实现
├── retrieval/           # 检索策略（向量、关键词、混合）
└── consolidation/       # 记忆整合与衰减
```

### 知识库 — `echo_agent/knowledge/`

文档提取、向量化存储、语义检索。

```
knowledge/
├── manager.py           # KnowledgeManager
├── extractors/          # 文档解析器（PDF、Word、Excel、PPT）
└── vector_store/        # 向量存储（FAISS、本地嵌入）
```

### 网关 — `echo_agent/gateway/`

HTTP/WebSocket 服务器，Dashboard API。

```
gateway/
├── server.py            # aiohttp 应用启动
├── auth.py              # JWT 认证
├── api/                 # REST API 模块
│   ├── sessions.py
│   ├── analytics.py
│   ├── config.py
│   └── ...
├── ws.py                # WebSocket 实时推送
└── static/              # 构建后的 Dashboard 静态文件
```

### 配置 — `echo_agent/config/`

Pydantic-settings 配置体系，支持 YAML/env/CLI 覆盖。

```
config/
├── schema.py            # 配置 Pydantic 模型（ProviderConfig 等）
├── loader.py            # 配置加载与合并
├── migration.py         # 版本迁移
└── docgen.py            # 自动生成配置参考文档
```

### 插件 — `echo_agent/plugins/`

插件发现、加载、manifest 权限预检、生命周期钩子。

```
plugins/
├── manifest.py          # PluginManifest（plugin.yaml 解析）
├── loader.py            # 插件发现与加载
├── manager.py           # PluginManager — 激活/停用
├── hooks.py             # HookRegistry — 生命周期钩子分发
├── sandbox.py           # manifest 权限声明与注册准入（非进程隔离）
├── context.py           # 插件执行上下文
└── errors.py            # 插件错误类型
```

### 其他子系统

| 目录 | 职责 |
|------|------|
| `a2a/` | Agent-to-Agent 协议 |
| `bus/` | 事件总线（InboundEvent/OutboundEvent） |
| `checkpoint/` | 文件检查点持久化 |
| `cli/` | CLI 入口、inline scrollback renderer、TUI（Textual） |
| `cost/` | 成本追踪与预算控制 |
| `dependencies/` | 依赖管理 |
| `evaluation/` | 评估框架（数据集、指标、Runner） |
| `evolution/` | 自我演进机制 |
| `mcp/` | MCP 客户端协议 |
| `media/` | 媒体处理（图片、音频） |
| `observability/` | 日志（loguru）、监控、OpenTelemetry |
| `permissions/` | 权限系统 |
| `scheduler/` | 定时任务调度 |
| `security/` | 安全策略、工具权限、命令过滤 |
| `session/` | 会话管理 |
| `skills/` | Skill Manager |
| `spill/` | 长输出溢出机制 |
| `storage/` | SQLite + 文件存储 |
| `tasks/` | 任务/工作流管理 |
| `utils/` | 通用工具函数 |
| `validation/` | 输入验证 |

## 前端结构 — `web/`

```
web/
├── src/
│   ├── main.tsx         # 入口
│   ├── App.tsx          # 路由配置
│   ├── pages/           # 页面组件（Overview、Sessions、Channels 等）
│   ├── components/      # 通用组件
│   ├── stores/          # Zustand 状态管理
│   ├── hooks/           # 自定义 React Hooks
│   ├── i18n/            # 国际化（i18next）
│   ├── lib/             # 工具库
│   └── test/            # 测试工具
├── package.json         # 依赖声明
├── vite.config.ts       # Vite 配置
└── tailwind.config.ts   # Tailwind CSS 配置
```

## Skills 目录 — `skills/`

按领域分类的内置 Skill 集合：

```
skills/
├── creative/            # 创意类
├── development/         # 开发工具
├── devops/              # 运维自动化
├── finance/             # 财务/金融
├── health/              # 健康管理
├── learning/            # 学习辅助
├── media/               # 多媒体处理
├── productivity/        # 生产力工具
├── research/            # 研究/分析
└── utility/             # 通用工具（calculator 等）
```
