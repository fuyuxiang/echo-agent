# Repository Map

Echo Agent uses a modular monorepo architecture with a Python backend package `echo_agent/` and an independent frontend SPA `web/`.

## Top-Level Structure

```
echo-agent/
├── echo_agent/          # Python main package
├── web/                 # Dashboard frontend (React + Vite)
├── skills/              # Built-in Skill collection
├── scripts/             # Install/publish scripts
├── tests/               # pytest test suite
├── docs/                # MkDocs documentation source
├── pyproject.toml       # Build config, dependencies, tool settings
└── .github/workflows/   # CI (lint, test, security, dashboard, docs, package)
```

## Core Subsystems

### Agent Core — `echo_agent/agent/`

Main agent loop, tool execution, planning, multi-agent collaboration.

```
agent/
├── loop.py              # AgentLoop — core reasoning-execution loop
├── planning/            # Task planning and decomposition
├── multi_agent/         # Multi-agent collaboration (delegate/spawn)
├── tools/               # Tool implementations (shell, filesystem, search, etc.)
│   ├── base.py          # → re-exports from echo_agent/tools/base.py
│   ├── registry.py      # ToolRegistry — registration, permission checks, audit
│   ├── shell.py         # ShellTool (exec) — command execution
│   ├── filesystem.py    # File read/write
│   ├── search.py        # Search tool
│   ├── memory.py        # Memory operation tools
│   ├── knowledge.py     # Knowledge base query tool
│   ├── skill_run.py     # Skill invocation
│   ├── delegate.py      # Multi-agent delegation
│   └── ...              # 30+ tool implementations
├── executors/           # Executor abstraction (process, container)
└── proc_lifecycle.py    # Subprocess lifecycle management
```

### Model Layer — `echo_agent/models/`

Multi-provider abstraction, routing, rate control, credential pooling.

```
models/
├── provider.py          # LLMProvider abstract base, LLMResponse, ToolCallRequest
├── providers/
│   ├── __init__.py      # Provider factory + _PROVIDER_MAP registry
│   ├── openai_provider.py
│   ├── anthropic_provider.py
│   ├── bedrock_provider.py
│   ├── gemini_provider.py
│   └── openrouter_provider.py
├── router.py            # Model router (task → Provider mapping)
├── rate_limiter.py      # Token bucket rate limiting
└── credential_pool.py   # Multi-key rotation
```

### Channel Layer — `echo_agent/channels/`

14 messaging channel adapters + manager.

```
channels/
├── base.py              # BaseChannel abstract base class
├── manager.py           # ChannelManager — start/stop, routing, delivery
├── cli.py               # CLI channel
├── telegram.py          # Telegram Bot
├── discord.py           # Discord Bot
├── slack.py             # Slack App
├── weixin.py            # WeChat Official Account
├── wecom.py             # WeCom (Enterprise WeChat)
├── feishu.py            # Feishu (Lark)
├── dingtalk.py          # DingTalk
├── email.py             # Email channel
├── webhook.py           # Generic Webhook
├── cron.py              # Scheduled triggers
├── matrix.py            # Matrix protocol
├── qqbot.py             # QQ Bot
└── whatsapp.py          # WhatsApp Business
```

### Memory System — `echo_agent/memory/`

Four-tier memory architecture: working memory, short-term, long-term, archive.

```
memory/
├── manager.py           # MemoryManager — unified interface
├── tiers/               # Four-tier storage implementations
├── retrieval/           # Retrieval strategies (vector, keyword, hybrid)
└── consolidation/       # Memory consolidation and decay
```

### Knowledge Base — `echo_agent/knowledge/`

Document extraction, vectorized storage, semantic retrieval.

```
knowledge/
├── manager.py           # KnowledgeManager
├── extractors/          # Document parsers (PDF, Word, Excel, PPT)
└── vector_store/        # Vector storage (FAISS, local embeddings)
```

### Gateway — `echo_agent/gateway/`

HTTP/WebSocket server, Dashboard API.

```
gateway/
├── server.py            # aiohttp application startup
├── auth.py              # JWT authentication
├── api/                 # REST API modules
│   ├── sessions.py
│   ├── analytics.py
│   ├── config.py
│   └── ...
├── ws.py                # WebSocket real-time push
└── static/              # Built Dashboard static files
```

### Configuration — `echo_agent/config/`

Pydantic-settings configuration system with YAML/env/CLI override support.

```
config/
├── schema.py            # Config Pydantic models (ProviderConfig, etc.)
├── loader.py            # Config loading and merging
├── migration.py         # Version migration
└── docgen.py            # Auto-generate config reference docs
```

### Plugins — `echo_agent/plugins/`

Plugin discovery, loading, sandbox, lifecycle hooks.

```
plugins/
├── manifest.py          # PluginManifest (plugin.yaml parsing)
├── loader.py            # Plugin discovery and loading
├── manager.py           # PluginManager — activate/deactivate
├── hooks.py             # HookRegistry — lifecycle hook dispatch
├── sandbox.py           # Plugin sandbox isolation
├── context.py           # Plugin execution context
└── errors.py            # Plugin error types
```

### Other Subsystems

| Directory | Responsibility |
|-----------|---------------|
| `a2a/` | Agent-to-Agent protocol |
| `bus/` | Event bus (InboundEvent/OutboundEvent) |
| `checkpoint/` | File checkpoint persistence |
| `cli/` | CLI entry point, TUI (Textual) |
| `cost/` | Cost tracking and budget control |
| `dependencies/` | Dependency management |
| `evaluation/` | Evaluation framework (datasets, metrics, runner) |
| `evolution/` | Self-evolution harness |
| `mcp/` | MCP client protocol |
| `media/` | Media processing (images, audio) |
| `observability/` | Logging (loguru), monitoring, OpenTelemetry |
| `permissions/` | Permission system |
| `scheduler/` | Job scheduler |
| `security/` | Security profiles, tool policies, command filtering |
| `session/` | Session management |
| `skills/` | Skill Manager |
| `spill/` | Long output spill mechanism |
| `storage/` | SQLite + file storage |
| `tasks/` | Task/workflow management |
| `utils/` | General utilities |
| `validation/` | Input validation |

## Frontend Structure — `web/`

```
web/
├── src/
│   ├── main.tsx         # Entry point
│   ├── App.tsx          # Route configuration
│   ├── pages/           # Page components (Overview, Sessions, Channels, etc.)
│   ├── components/      # Shared components
│   ├── stores/          # Zustand state management
│   ├── hooks/           # Custom React Hooks
│   ├── i18n/            # Internationalization (i18next)
│   ├── lib/             # Utility libraries
│   └── test/            # Test utilities
├── package.json         # Dependency declarations
├── vite.config.ts       # Vite configuration
└── tailwind.config.ts   # Tailwind CSS configuration
```

## Skills Directory — `skills/`

Built-in Skills organized by domain:

```
skills/
├── creative/            # Creative tasks
├── development/         # Development tools
├── devops/              # DevOps automation
├── finance/             # Finance
├── health/              # Health management
├── learning/            # Learning assistance
├── media/               # Multimedia processing
├── productivity/        # Productivity tools
├── research/            # Research/analysis
└── utility/             # General utilities (calculator, etc.)
```
