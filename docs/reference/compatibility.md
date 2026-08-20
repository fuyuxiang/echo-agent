# 兼容性参考

Echo Agent 的平台支持、依赖要求和版本兼容信息。

## 平台支持

### 操作系统

| 平台 | 支持状态 | 说明 |
|------|----------|------|
| Linux (x86_64) | 完全支持 | 推荐用于生产环境 |
| Linux (ARM64) | 完全支持 | 含 Raspberry Pi 4+ |
| macOS (Apple Silicon) | 完全支持 | M1/M2/M3 |
| macOS (Intel) | 完全支持 | 10.15+ |
| Windows (WSL2) | 完全支持 | 推荐方式 |
| Windows (原生) | 部分支持 | 部分工具受限 |

!!! warning "Windows 原生模式"
    Windows 原生模式下以下功能受限：

    - `shell` 工具：使用 PowerShell/cmd 替代 bash
    - Unix Socket：不可用，仅支持 TCP
    - 文件权限：无 POSIX 权限控制
    - 信号处理：部分信号不可用

    建议使用 WSL2 以获得完整功能。

### 容器化

| 环境 | 支持状态 | 说明 |
|------|----------|------|
| Docker | 完全支持 | 提供官方镜像 |
| Podman | 完全支持 | 兼容 Docker 镜像 |
| Kubernetes | 完全支持 | 提供 Helm Chart |

---

## Python 版本

`pyproject.toml` 声明 `requires-python = ">=3.11"`。

| Python 版本 | 支持状态 | 说明 |
|-------------|----------|------|
| 3.11 | 完全支持 | 最低要求，CI 覆盖 |
| 3.12 | 完全支持 | CI 覆盖 |
| 3.13 及以上 | 未验证 | 满足 `requires-python` 因此可以安装，但不在 CI 矩阵内，也未在 PyPI classifier 中声明 |
| 3.10 及以下 | 不支持 | 低于 `requires-python`，pip 会拒绝安装 |

"完全支持"的判据是 CI 在该版本上跑完整测试套件。目前矩阵为 3.11 与 3.12 两个版本；在更高版本上运行属于可行但未经验证的状态，遇到问题欢迎提 issue。

---

## 核心依赖

### 运行时依赖

| 依赖 | 最低版本 | 用途 |
|------|----------|------|
| `pydantic` | 2.0 | 配置校验与数据模型 |
| `httpx` | 0.25 | HTTP 客户端 |
| `websockets` | 12.0 | WebSocket 服务与客户端 |
| `uvicorn` | 0.24 | ASGI 服务器 |
| `starlette` | 0.27 | Web 框架（Gateway） |
| `loguru` | 0.7 | 日志系统 |
| `rich` | 13.0 | TUI 终端渲染 |
| `textual` | 0.40 | TUI 框架 |
| `aiosqlite` | 0.19 | 异步 SQLite |
| `click` | 8.1 | CLI 框架 |

### 可选依赖

| 依赖 | 用途 | 安装方式 |
|------|------|----------|
| `opentelemetry-sdk` | OTel 追踪与指标 | `pip install echo-agent[otel]` |
| `asyncpg` | PostgreSQL 后端 | `pip install echo-agent[postgres]` |
| `chromadb` | 向量数据库 | `pip install echo-agent[knowledge]` |
| `sentence-transformers` | 本地 Embedding | `pip install echo-agent[embeddings]` |
| `playwright` | 浏览器自动化 | `pip install echo-agent[browser]` |

### 安装组合

| 组合 | 命令 | 包含 |
|------|------|------|
| 最小安装 | `pip install echo-agent` | 核心功能 |
| 完整安装 | `pip install echo-agent[all]` | 所有可选依赖 |
| 开发环境 | `pip install echo-agent[dev]` | + 测试与开发工具 |

---

## 模型提供商兼容性

### 支持的提供商

| 提供商 | 支持模型 | 流式输出 | 工具调用 | 视觉 |
|--------|----------|----------|----------|------|
| Anthropic | Claude 系列 | ✓ | ✓ | ✓ |
| OpenAI | GPT-4 / GPT-4o | ✓ | ✓ | ✓ |
| Google | Gemini 系列 | ✓ | ✓ | ✓ |
| 本地（Ollama） | 兼容模型 | ✓ | 部分 | 部分 |
| OpenAI 兼容 API | 各类自部署 | ✓ | 取决于后端 | 取决于后端 |

### 模型功能矩阵

| 功能 | 要求 | 说明 |
|------|------|------|
| 基础对话 | 任意 LLM | 最低要求 |
| 工具调用 | 支持 function calling | 核心功能 |
| 流式输出 | 支持 SSE/streaming | 提升交互体验 |
| 视觉理解 | 多模态模型 | vision 工具需要 |
| 长上下文 | 100K+ token | 复杂任务推荐 |

---

## 通道集成兼容性

### Slack

| 要求 | 说明 |
|------|------|
| API 版本 | Slack Web API v2 |
| 权限 | `chat:write`, `channels:read`, `files:read`, `files:write` |
| Socket Mode | 需要 App-Level Token |
| Events | 需要 Event Subscriptions |

### Telegram

| 要求 | 说明 |
|------|------|
| Bot API 版本 | 6.0+ |
| 功能 | 长轮询 / Webhook 均支持 |
| 文件限制 | 上传最大 50MB，下载最大 20MB |

### Discord

| 要求 | 说明 |
|------|------|
| API 版本 | v10 |
| Gateway Intent | `MESSAGE_CONTENT`, `GUILD_MESSAGES` |
| 权限 | Send Messages, Read History, Attach Files |

---

## 浏览器支持（Dashboard）

| 浏览器 | 最低版本 | 说明 |
|--------|----------|------|
| Chrome / Edge | 90+ | 推荐 |
| Firefox | 90+ | 完全支持 |
| Safari | 15+ | 完全支持 |
| Mobile Chrome | 90+ | 响应式支持 |
| Mobile Safari | 15+ | 响应式支持 |

---

## 网络要求

### 出站连接

| 目标 | 端口 | 用途 | 必需 |
|------|------|------|------|
| api.anthropic.com | 443 | Anthropic API | 使用 Claude 时 |
| api.openai.com | 443 | OpenAI API | 使用 GPT 时 |
| generativelanguage.googleapis.com | 443 | Google AI API | 使用 Gemini 时 |
| slack.com / wss-primary.slack.com | 443 | Slack 通道 | 使用 Slack 时 |
| api.telegram.org | 443 | Telegram 通道 | 使用 Telegram 时 |
| discord.com / gateway.discord.gg | 443 | Discord 通道 | 使用 Discord 时 |

### 入站连接

| 端口 | 用途 | 说明 |
|------|------|------|
| 8080（默认） | Gateway API + WebSocket | 可配置 |
| 9090（默认） | Prometheus Metrics | 可选，启用 OTel 时 |

---

## 已知限制

| 限制 | 说明 | 影响 |
|------|------|------|
| SQLite 并发写 | 单写多读 | 高并发场景建议用 PostgreSQL |
| 文件监控 | 依赖平台 inotify/FSEvents | 容器内可能需要轮询 |
| 内存使用 | 长上下文会话占用较多 RAM | 建议 2GB+ 可用内存 |
| GPU | 本地 Embedding 模型可能需要 | CPU 也可运行（较慢） |

---

## 升级兼容性

### 版本策略

Echo Agent 遵循语义版本号：`major.minor.patch`

| 变更类型 | 兼容性保证 |
|----------|-----------|
| Patch（0.3.x） | 完全向后兼容 |
| Minor（0.x.0） | 配置兼容，可能需数据迁移 |
| Major（x.0.0） | 可能有破坏性变更 |

### 数据迁移

版本升级时如需数据迁移，使用：

```bash
echo-agent migrate status   # 查看待执行的迁移
echo-agent migrate run      # 执行迁移
echo-agent migrate rollback # 出问题时回滚
```

`migrate run` 支持 `--dry-run`，先预演再执行。

!!! danger "备份数据库文件，而非依赖检查点"
    执行 `migrate run` 前请直接复制 `data/echo_agent.db`。`echo-agent checkpoint` 是工作区**文件**的影子 Git 快照，其排除范围包含数据库、会话与记忆目录，无法用于恢复迁移后的数据。
