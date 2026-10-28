# 仓库地图

服务端是 `echo-agent-runtime/` 下的独立工程，单独下载即包含 Python 源码、Dashboard 源码、内置技能、构建脚本和部署模板。桌面端 `client/`、文档 `docs/` 和仓库工作流位于根目录。下表除明确标注的仓库级条目外，路径均相对于 `echo-agent-runtime/`。

| 路径 | 职责 |
|---|---|
| `echo_agent/app.py` | 启动与停止顺序、配置和子系统装配 |
| `echo_agent/__main__.py` | CLI 参数解析与主命令分发 |
| `echo_agent/agent/loop.py` | Agent 主循环 |
| `echo_agent/agent/pipeline/` | 推理与工具调用阶段 |
| `echo_agent/agent/tools/` | 内置工具实现和注册 |
| `echo_agent/agent/executors/` | 本地、工作目录副本、容器或远程执行后端 |
| `echo_agent/agent/planning/` | 任务规划 |
| `echo_agent/agent/multi_agent/` | 多 Agent 协作 |
| `echo_agent/tools/` | 供扩展使用的工具公共入口 |
| `echo_agent/models/` | 模型路由、凭证池与 Provider 抽象 |
| `echo_agent/models/providers/` | 模型供应商实现 |
| `echo_agent/channels/` | 消息通道适配器及管理器 |
| `echo_agent/memory/` | 记忆存储、检索与遗忘策略 |
| `echo_agent/knowledge/` | 文档抽取、索引及向量存储 |
| `echo_agent/gateway/server.py` | aiohttp Gateway 和核心路由 |
| `echo_agent/gateway/api/` | 管理 API 处理器及路由注册 |
| `echo_agent/gateway/auth.py` | API 令牌和配对授权 |
| `echo_agent/config/schema.py` | Pydantic 配置模型与 schema 默认值 |
| `echo_agent/config/default.yaml` | 包内配置覆盖值 |
| `echo_agent/config/loader.py` | 配置文件、环境变量与覆盖值合并 |
| `echo_agent/config/docgen.py` | 配置参考生成 |
| `echo_agent/storage/sqlite.py` | SQLite 连接与自动表结构迁移 |
| `echo_agent/checkpoint/` | 影子 Git 工作区文件检查点 |
| `echo_agent/cli/` | 终端客户端、后台服务管理与 CLI 子命令 |
| `echo_agent/plugins/` | 插件发现、清单准入与生命周期 |
| `echo_agent/mcp/` | MCP 客户端 |
| `echo_agent/observability/` | 内部轨迹、日志缓冲与可选 OpenTelemetry |
| `echo_agent/skills/` | 技能加载与运行时管理 |
| `echo_agent/tasks/` | 任务管理 |
| `echo_agent/artifacts/` | 用户产物 |
| `web/src/` | Dashboard React 前端源码 |
| `skills/` | 按领域组织的内置 SKILL.md 文件与脚本 |
| `tests/` | pytest 测试 |
| 仓库根目录 `docs/` | MkDocs 文档 |
| 仓库根目录 `scripts/tests/` | 文档与代码一致性检查 |
| `deploy/` | 独立服务端配置和容器模板 |
| `scripts/` | 安装、构建与发布脚本 |

## 关键关系

`echo_agent/app.py` 将配置、存储、模型、Agent、通道和 Gateway 连接起来。`echo_agent/tools/` 是工具扩展的公共入口；`echo_agent/agent/tools/` 实现内置工具。Gateway 使用 `aiohttp` 和令牌/配对授权，不使用 JWT 服务端或 ASGI 框架。SQLite 表结构迁移在 `echo_agent/storage/sqlite.py` 初始化连接时运行；`echo-agent migrate` 位于 `echo_agent/cli/migrate_cmd.py`，处理的是记忆数据。
