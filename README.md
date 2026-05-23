<div align="center">

# Echo Agent

**自托管的长期运行 Agent 运行时 —— 技能可在运行轨迹中持续演化，记忆可跨越会话边界沉淀。**

[中文](README.md) · [English](README.en.md)

[![Python](https://img.shields.io/badge/python-3.11%2B-3776AB.svg?logo=python&logoColor=white)](https://www.python.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](pyproject.toml)
[![Status](https://img.shields.io/badge/status-alpha-f59e0b.svg)](#项目状态)
[![Self-hosted](https://img.shields.io/badge/self--hosted-✓-111827.svg)](#架构)

[快速开始](#快速开始) · [自进化](#自进化) · [记忆系统](#记忆系统) · [架构](#架构)

</div>

---

## 简介

Echo Agent 是一个面向私有部署的 Agent 运行时。除模型 API 外，轨迹、记忆、会话与凭证均落于本地，无外部数据回传。

与多数将"工具调用循环"作为终点的 Agent 框架不同，Echo Agent 把每一次任务执行视为可学习的样本：

- **运行时改进闭环**。任务以结构化 Trajectory 写入持久层；Evolver 基于失败与低分轨迹由 LLM 提议候选技能变更；候选先在评测集上完成 baseline / candidate 的 A/B 对比，仅当指标严格优于 baseline 时方可晋升进入技能库，并支持一键回滚。
- **分级记忆**。Working / Episodic / Semantic / Archival 四层结构，配合 Ebbinghaus 自适应衰减、BM25 与向量混合检索、版本化记忆格的矛盾检测，使记忆具备时序敏感性与可验证性。
- **多入口同源**。CLI、Webhook、Cron、Gateway 与 12+ 消息通道（Telegram / Discord / Slack / 微信 / QQ / 飞书 等）共享同一消息总线、Agent Loop、记忆与权限边界。
- **多模型 provider**。OpenAI、Anthropic Claude、Google Gemini、AWS Bedrock、OpenRouter 及任何 OpenAI 兼容端点。

> **当前阶段**：Alpha。配置字段、内部存储 schema 与 API 在稳定版前可能调整。

---

## 设计动机

主流 Agent 框架的能力边界在部署时即被冻结：运行时观测到的失败模式无法反向作用于技能定义；上下文窗口的硬截断也不构成长期记忆。Echo Agent 围绕两个长期被忽略的问题重建底层抽象：

1. **能力是否会随运行时间增长？**
2. **记忆是否能跨越会话边界沉淀？**

第一点由自进化引擎承担——记录、反思、提议、评测、晋升、冷却的完整闭环；第二点由分级记忆系统承担——分层存储、自适应衰减、混合检索、矛盾检测、sleep-time 整理。两者构成核心反馈回路：记忆系统为进化引擎提供高质量样本，进化引擎反向提升轨迹质量与技能可用性。

---

## 核心特性

| 模块 | 描述 |
|------|------|
| **自进化技能库** | Trajectory → Evolver → A/B 评测 → 晋升或驳回，支持回滚与冷却 |
| **四层记忆系统** | Working / Episodic / Semantic / Archival，混合检索 + 自适应遗忘 |
| **多 Agent 协作** | planner / coder / researcher / operator 角色按任务路由，支持并行执行 |
| **统一消息总线** | CLI、Webhook、Cron、Gateway 与 12+ 消息通道共享同一 Agent Loop |
| **细粒度权限** | 高风险工具默认进入审批流，支持 LLM 风险评估、路径策略、人工管理员 |
| **A2A + MCP** | 实现 Google A2A 协议；集成 Anthropic MCP，可挂载任意 MCP server |
| **多模型 provider** | OpenAI / Anthropic / Gemini / Bedrock / OpenRouter 及 OpenAI 兼容端点 |
| **私有部署** | 轨迹、记忆、会话与凭证均落于本地 SQLite 与文件系统 |

---

## 快速开始

### 环境要求

- Python **3.11+**
- Linux、macOS 或 WSL2
- 至少一个模型 provider 的 API key
- 推荐使用 [`uv`](https://docs.astral.sh/uv/) 管理虚拟环境

### 源码安装

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

uv venv venv --python 3.11
source venv/bin/activate
uv pip install -e ".[all]"

echo-agent setup -w .   # 交互式配置向导
echo-agent run -w .     # 前台启动
```

### 一键脚本（仅供开发环境）

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh         # 建议先审阅脚本
bash install.sh
```

> 脚本会写入 `PATH` 并在 Linux 下安装 systemd 服务。生产部署请采用源码安装路径，自行管理虚拟环境与服务注册。

### 第一次运行

```bash
# 启动交互式 CLI
echo-agent

# 在 CLI 中提交一条任务
> 帮我写一个监控服务器剩余磁盘的 Python 脚本，保存为 disk_check.py

# 任务结束后，本次轨迹自动写入 SQLite，可被后续 evolution 消费
echo-agent evolution status
```

---

## 自进化

> 把每一次任务执行转化为可验证的改进信号。候选不直接生效，先评测、再晋升、可回滚。

### 闭环管线

```text
   ┌──────────────────────┐
   │  TrajectoryRecorder  │  捕获 Agent Loop 中每次任务的完整轨迹
   └──────────┬───────────┘
              ▼
   ┌──────────────────────┐
   │       Evolver        │  LLM 基于轨迹提议候选技能变更
   └──────────┬───────────┘
              ▼
   ┌──────────────────────┐
   │    PromotionGate     │  baseline / candidate A/B 评测
   └──────────┬───────────┘
              │
       ┌──────┴──────┐
       ▼             ▼
   ┌────────┐   ┌─────────┐
   │ 晋升   │   │ 驳回    │
   │ + 冷却 │   │ + 还原  │
   └───┬────┘   └─────────┘
       │
       └─→ 反馈至下一轮任务执行
```

### 关键约束

- **先评测后生效**：候选不直接覆盖技能库；先对技能目录做快照，在隔离副本中应用候选，再分别在 baseline 与 candidate 上跑评测。
- **回归阈值守门**：评测指标退步超过 `regression_threshold` 的候选直接驳回；`require_strict_improvement` 开启时与 baseline 持平亦视为不通过。
- **冷却期**：晋升后默认进入 24 小时冷却，避免短期内重复变更同一技能。
- **一键回滚**：`echo-agent evolution rollback <skill>` 还原最近一次晋升。
- **完整审计**：Trajectory、SkillCandidate、EvolutionRun 全部持久化，候选可追溯至源轨迹。
- **人工档**：`auto_promote: false` 时，候选进入 `needs_review`，需 `evolution promote <id>` 显式晋升。

### 启用配置

```yaml
evolution:
  enabled: true
  trigger_mode: "threshold"            # manual | threshold | scheduled
  threshold_trajectories: 50
  cron_expression: "0 4 * * *"
  max_candidates_per_run: 3
  max_trajectories_per_run: 200
  eval_dataset_path: "data/eval/baseline.yaml"
  regression_threshold: 0.05
  require_strict_improvement: true
  auto_promote: false                  # 生产建议先关闭，人工审核
  candidate_review_required: true
  cooldown_seconds_after_promote: 86400
  trajectory_retention_days: 30
  skill_size_limit_bytes: 50000
  redact_args: true
```

### CLI

```bash
echo-agent evolution status              # 引擎状态、待审候选、最近一次 run
echo-agent evolution run                 # 手动触发一次完整 evolution pass
echo-agent evolution list-candidates     # --status pending|promoted|rejected|needs_review
echo-agent evolution show-candidate <id> # rationale、预期改进、A/B 报告
echo-agent evolution promote <id>        # 手动晋升一个 needs_review 候选
echo-agent evolution rollback <skill>    # 回滚某技能的最近一次晋升
echo-agent evolution init-dataset        # 初始化 baseline 评测数据集
```

候选格式、评分细节与回滚行为的完整设计文档：TODO（`docs/` 目前仅包含架构图，详细文档待补全）。

---

## 记忆系统

四层分级架构，对短期到长期的记忆进行差异化存储与检索；目标是在有限存储与有限上下文窗口下，对用户偏好、领域事实与历史经验提供时序敏感、语义可验证的持久化能力。

### 分层

| 层级 | 用途 | 持久化 |
|------|------|--------|
| **Working** | 当前对话的进程内缓冲，容量受限（默认 20 条） | 否 |
| **Episodic** | 对话片段摘要，按会话与时间索引 | SQLite |
| **Semantic** | 从情节中提炼的核心事实，主要持久层 | SQLite + 向量索引 |
| **Archival** | 重要性衰减至阈值以下的归档，进一步衰减后清除 | SQLite |

### 检索

`HybridRetriever` 融合 BM25 关键词匹配与 FAISS 向量相似度，按查询熵自适应调整两者权重（Resonance Scoring）：模糊查询偏向向量召回，精确查询偏向关键词。Ebbinghaus 衰减因子作为权重参与重排阶段。

向量索引基于 FAISS（可选依赖），未安装时自动降级为纯关键词检索。

### 遗忘曲线

自适应衰减遵循 Ebbinghaus 公式：

```
half_life = base × (1 + log₂(1 + access_count))
```

访问次数越多，半衰期越长，遗忘越慢。有效重要性低于归档阈值时降级至 Archival；低于遗忘阈值时彻底清除。

### 矛盾检测

新记忆写入时通过版本化记忆格（versioned memory lattice）检测与已有记忆的语义冲突，支持 LLM 验证与启发式（同 key 不同内容）两种模式。**冲突不被静默覆盖，而是作为时序边写入图谱**，保留完整信念变迁链。

### 整理与审查

会话结束后，`MemoryConsolidator` 依次执行：摘要写入 → 创建情节 → 提取语义事实并提升 → 矛盾检测 → 遗忘与归档扫描。`MemoryReviewer` 在非平凡对话后由 LLM 判定是否持久化用户偏好、项目事实或经验教训，并执行 add / replace / remove。

### 记忆类别

| 类型 | 说明 |
|------|------|
| **user** | 偏好、习惯、沟通风格、个人上下文。按会话隔离，带 `global` 标签时跨会话可见 |
| **environment** | 项目事实、工具配置、流程规则、领域知识。全局可见 |

---

## 配置

按以下优先级加载：`-c` 参数指定的文件 > 工作区中的 `echo-agent.yaml` > `~/.echo-agent/echo-agent.yaml`。

### 最小可用配置

```yaml
workspace: "~/.echo-agent"

models:
  defaultModel: "gpt-4o-mini"
  providers:
    - name: "openai"
      apiKeyEnv: "OPENAI_API_KEY"      # 从环境变量读取，避免明文落库

channels:
  cli:
    enabled: true

permissions:
  adminUsers:
    - "cli_user"

evolution:
  enabled: true
  trigger_mode: "threshold"
  auto_promote: false
```

```bash
export OPENAI_API_KEY="sk-..."
echo-agent run -w .
```

> 避免将 API key 直接写入 `echo-agent.yaml`。建议使用环境变量、本地 only 的覆盖文件，或外部密钥管理系统（Vault、AWS Secrets Manager 等）。

### Provider 与路由

支持的 provider：`openai`、`anthropic`、`gemini` / `google`、`bedrock` / `aws`、`openrouter`，以及任何 OpenAI 兼容端点。模型路由支持按任务类型匹配、fallback 策略与凭证池轮换。

### 环境变量

`ECHO_AGENT_` 前缀的环境变量会按双下划线 `__` 拆解为嵌套配置项并注入运行时配置。

| 名称 | 是否必需 | 默认值 | 说明 |
|------|----------|--------|------|
| `ECHO_AGENT_CREDENTIAL_KEY` | 否 | 未设置 | 用于本地凭证存储加密的对称密钥；`credentials.requireEncryption: true`（默认）下若未设置，凭证落库会受限。环境变量名可由 `credentials.encryptionKeyEnv` 修改 |
| `ECHO_HOME` | 否 | `~/.echo-agent` | 安装脚本与默认运行使用的工作区根目录 |
| `ECHO_INSTALL_DIR` | 否 | `$ECHO_HOME/echo-agent` | 安装脚本的源码克隆目录 |
| `ECHO_COMMAND_LINK_DIR` | 否 | `~/.local/bin` 或 `/usr/local/bin` | 安装脚本写入 `echo-agent` 软链接的目录 |
| `ECHO_AGENT_<SECTION>__<KEY>` | 否 | — | 任意配置覆盖，例如 `ECHO_AGENT_GATEWAY__PORT=9000` |

模型 provider 的 API key 由 `models.providers[].apiKey`（不推荐明文）或对应 provider 的环境变量读取（例如 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY` 等，由 SDK 自身约定）。仓库未提供 `.env.example`：TODO 列出每个 provider 的标准环境变量名。

---

## 命令

```bash
echo-agent                    # 交互式 CLI
echo-agent run                # 前台运行
echo-agent setup              # 完整配置向导（含 evolution 子向导）
echo-agent setup model        # 仅配置模型与 provider
echo-agent setup channel      # 仅配置消息通道
echo-agent status             # 当前配置与运行状态
echo-agent gateway            # 启动 Gateway 服务
echo-agent eval -d eval.yaml  # 运行评测数据集
echo-agent plugin list        # 查看已加载插件
```

服务管理（仅 Linux）：

```bash
echo-agent service install
echo-agent service start
echo-agent service status
echo-agent service logs
echo-agent service uninstall
```

---

## Gateway

Gateway 提供 HTTP / WebSocket 接口，适用于自定义前端、内部系统、自动化脚本与外部 Agent 集成。根路径提供内置 Playground 用于本地调试。

```bash
echo-agent gateway --host 127.0.0.1 --port 9000
```

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/` | 内置 Playground |
| `GET` | `/api/v1/health` | 健康检查 |
| `POST` | `/api/v1/message` | 发送消息至 Agent |
| `GET` | `/api/v1/sessions` | 会话列表 |
| `DELETE` | `/api/v1/sessions/{key}` | 重置 Gateway 会话 |
| `POST` | `/api/v1/pair` | 生成配对码 |
| `POST` | `/api/v1/pair/verify` | 验证配对码 |
| `GET` | `/api/v1/stats` | 运行统计 |
| `GET` | `/ws` | WebSocket 接口 |
| `GET` | `/.well-known/agent.json` | A2A Agent Card |
| `POST` | `/a2a` | A2A JSON-RPC 入口 |

认证支持 `open`、`allowlist`、`pairing` 三种模式，API token 可通过 `X-Echo-Agent-Token` 或 `Authorization: Bearer` 传入。**公网部署必须启用认证与网络层访问控制**。

---

## 通道

所有通道输入均规范化为统一消息事件，进入同一消息总线与 Agent Loop。CLI、Telegram、微信、QQBot、Gateway 等不同来源的请求共享一致的会话、记忆、工具与权限边界，并使用同一套持续演化的技能库。

| 分类 | 通道 |
|------|------|
| 本地与系统 | `cli`、`webhook`、`cron` |
| 国际平台 | `telegram`、`discord`、`slack`、`whatsapp`、`email`、`matrix` |
| 国内生态 | `wechat` / `weixin`、`qqbot`、`feishu`、`dingtalk`、`wecom` |

各通道稳定性受第三方 API 政策与适配器实现差异影响，详见 [限制](#限制)。

---

## 工具

内置工具按类别组织，由权限与审批系统统一管控；MCP server 可按配置动态注册外部工具。

| 分类 | 工具 |
|------|------|
| 工作区 | `read_file`、`write_file`、`edit_file`、`list_dir`、`search_files`、`patch` |
| 执行 | `exec`、`execute_code`、`process` |
| Web | `web_fetch`、`web_search` |
| 协作 | `message`、`notify`、`clarify`、`delegate_task`、`spawn_task` |
| 记忆与会话 | `session_search`、`memory` |
| 任务与工作流 | `todo`、`task`、`workflow`、`cronjob` |
| 技能 | `skills_list`、`skill_view`、`skill_manage`、`skill_install` |
| 多模态 | `vision_analyze`、`text_to_speech`、`image_generate` |
| 知识库 | `knowledge_search`、`knowledge_index` |
| MCP | 由配置中的 MCP server 动态注册 |

`exec`、`write_file`、`edit_file` 等高风险工具默认进入审批流，可通过 `permissions.adminUsers` 与 `permissions.approval` 调整访问控制与审批策略。审批支持 LLM 风险评估（Smart Mode）、路径策略与人工管理员协同。

---

## 技能

技能采用目录与 `SKILL.md` 的开放格式，内置 `arxiv`、`weather`、`summarize`、`plan`、`skill-creator` 等。支持查看、创建、修改、删除，亦可从本地路径、Git 仓库或 URL 安装。

技能库支持运行时自动演化，详见 [自进化](#自进化)。

---

## 架构

请求从 CLI、Gateway、调度器、Webhook 或某个通道适配器进入，被规范化为统一消息事件，经消息总线路由至 Agent Loop；执行过程经过模型路由、记忆检索、权限审批、工具调用与可观测性采集，最终落入轨迹记录器供进化引擎消费。

```text
Channel / CLI / Gateway / Webhook / Cron
                ↓
          Message Bus
                ↓
          Agent Loop
                ↓
   Context Builder + Memory Retriever
                ↓
         Planner / Router
                ↓
        Permission Gate
                ↓
   Tool Execution / Model Call
                ↓
     Trajectory Recorder
                ↓
      Evolution Pipeline
```

![架构图](https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/docs/assets/architecture.png)

> TODO：当前 `docs/assets/architecture.png` 为既有架构图，请在新增模块（如 evolution、plugins）落地后核对其是否仍与代码一致。

### 代码导航

```text
echo_agent/
├── a2a/            # A2A 协议（Agent-to-Agent 互操作）
├── agent/          # Agent loop、上下文构建、压缩、工具执行
├── bus/            # 消息事件队列
├── channels/       # CLI、消息通道、webhook、cron 适配器
├── cli/            # 配置向导、状态查看、服务管理、evolution 子命令
├── config/         # 配置 schema、加载器、默认值
├── evaluation/     # 评测数据集与 runner（供 baseline / candidate A/B 使用）
├── evolution/      # 自进化：轨迹记录、evolver、晋升 gate、调度器
├── gateway/        # HTTP / WebSocket Gateway
├── knowledge/      # 知识库索引与检索
├── mcp/            # MCP 客户端、传输层、OAuth
├── memory/         # 四层记忆、混合检索、遗忘曲线、矛盾检测、向量索引
├── models/         # Provider、路由、凭证池
├── observability/  # 健康检查、Span、遥测
├── permissions/    # 权限与凭证原语
├── plugins/        # 插件 hook，自进化模块由此接入 Agent Loop
├── scheduler/      # 计划任务服务
├── security/       # 风险分类、路径策略、LLM 安全审批
├── session/        # 会话持久化
├── skills/         # 技能存储与审查
├── storage/        # SQLite 后端
└── tasks/          # 任务管理与工作流引擎
```

---

## 项目状态

| 模块 | 状态 | 说明 |
|------|------|------|
| CLI 运行时 | Beta | 交互式命令行与前台运行已可用 |
| 配置与凭证 | Beta | 含交互式 setup 向导与多模型 provider |
| Gateway (REST / WebSocket) | Alpha | 公网部署须启用认证 |
| Self-Evolution | Experimental | 生产场景建议 `auto_promote: false`，人工审核晋升 |
| 四层记忆系统 | Experimental | FAISS 为可选依赖，未安装时降级为关键词检索 |
| A2A / MCP | Experimental | 协议本身仍在演进 |
| 消息通道 | Experimental | 各通道稳定性受第三方 API 与适配器质量影响 |
| 评测框架 | Experimental | 进化质量取决于评测数据集的覆盖度 |

---

## 安全模型

Echo Agent 默认作为开发者本地工具运行，对工作区与本机具有较广的访问权限。运维者需理解以下边界后再启用对外通道：

**Agent 可访问的资源**

- 文件系统：`read_file` / `write_file` / `edit_file` / `list_dir` / `search_files` / `patch` 默认作用于配置中的工作区目录；`tools.restrictToWorkspace` 与 `tools.safeWriteRoot` 可进一步收窄写入范围。
- Shell 与进程：`exec` 默认运行在 `tools.exec.host`（`sandbox`），并通过 `safeBins` / `allowedCommands` / `blockedCommands` 控制可执行的命令；`process` 与 `execute_code` 提供进程管理与代码执行能力。
- 网络：`web_fetch` / `web_search` 通过外部 HTTP 出网；`web` 工具默认启用，`execution.networkPolicy` 可调整为 `deny` 或 `restricted`。
- 模型 API：执行轨迹中的对话、工具参数与上下文摘要会被发送至所配置的模型 provider。

**凭证与密钥**

- API key 与 token 来源：`models.providers[].apiKey` / 环境变量 / 凭证存储（由 `credentials.encryptionKeyEnv` 指定的对称密钥加密）。
- 仓库不附带 `.env.example`，请勿将密钥写入 `echo-agent.yaml` 后提交版本库。
- 工具调用日志在持久化前会对包含 `key` / `token` / `secret` / `password` / `api_key` / `credential` / `auth` 字段名的参数做脱敏（`echo_agent/agent/tools/registry.py`）。

**权限与审批**

- `permissions.approval.requireApproval` 列表中的工具进入审批流程；默认包含 `cronjob`、`exec`、`execute_code`、`process`、`skill_install`、`skill_manage`。
- `permissions.approval.mode` 支持 `manual`、`smart`（LLM 风险分类）、`off`。
- `permissions.approval.unattendedPolicy` 控制无人值守通道（如 webhook）下默认是放行还是拒绝。
- `permissions.adminUsers` 列出可管理审批与高权限工具的用户；CLI 通道下 `cli_user` 通常应被列入。

**网络入口**

- Gateway（HTTP / WebSocket）默认 `auth.mode: allowlist`；公网部署须使用 `allowlist` 或 `pairing` 之一，并配合防火墙 / 反向代理收敛网络入口。
- 各消息通道（Telegram、Slack、QQBot、微信等）通过 `allow_from` 列表收敛可发起请求的用户。

**已知风险**

- LLM 提示词注入：来自外部内容（网页、文件、消息）的指令可能尝试篡改 Agent 行为；`memory` 写入路径包含注入扫描与不可见 Unicode 检查，但其他工具的输出未经统一过滤。
- 工具误用：写入与执行类工具一旦被授权，可能在错误前提下修改文件或执行命令，请保留审批与审计。
- 凭证外泄：日志已对常见敏感字段名脱敏，但提示词与模型响应不在脱敏范围；不要在对话中粘贴生产密钥。
- 自进化候选可能错误：默认 `auto_promote: true`，生产环境建议显式设为 `false` 并人工审核。

仓库未声明完整的安全审计或第三方渗透测试结论，**不应将其视为开箱即用的生产级安全基线**。

---

## 隐私

- **本地数据**：默认存储在 `workspace/data/` 下（由 `storage.databasePath` 与 `storage.*Dir` 决定），包括 SQLite 数据库（会话、记忆、轨迹、技能候选）、`memory/HISTORY.md` 与 `memory/MEMORY.md`、`logs/`、`media_cache/` 等。
- **远端数据**：除模型 provider API、Web 工具外发请求与（如启用的）OpenTelemetry 导出端点外，运行时不会主动外发数据。模型 provider 会接收对话内容、工具描述与工具参数。
- **可观测性**：`observability.otelEnabled` 默认为 `true`，但仅在配置了 `otelEndpoint` 时才会真正导出 trace；未配置端点时数据不会离开本机。
- **删除本地状态**：停止服务后删除 `workspace/data/` 即可清理持久化状态；亦可单独清理 `data/echo_agent.db`、`data/memory/`、`data/sessions/` 等子目录。
- **审计**：评测、自进化运行、多 Agent 委派等关键路径写入 `data/delegation_audit.jsonl` 等审计文件，便于事后追溯。

> 隐私与合规承诺取决于运维者的部署方式与所选 provider 的政策。仓库本身不提供隐私保证。

---

## 限制

- **自进化质量取决于评测数据集的覆盖度与代表性**。空数据集或样本过少时，A/B 对比将失去意义。
- **LLM 提议的候选技能可能错误**，生产部署建议关闭 `auto_promote`，由人工审核后晋升。
- **记忆抽取可能产生过时或不准确的事实**，关键决策应回到原始上下文核对。
- **Shell、文件编辑、进程控制与代码执行是高权限工具**，仅向受信用户开放；公网入口必须启用认证。
- **Gateway 公网部署须启用 `allowlist` 或 `pairing` 认证**，并配合网络层访问控制（防火墙 / 反向代理）。
- **通道适配器依赖第三方 API**，部分实现可能受限于 bot 政策或非官方协议。
- **内部存储 schema 与配置字段在稳定版前可能调整**，升级时请阅读 release notes。

---

## 运维建议

- 将 API key、token 与 `data/credentials.json` 存放于环境变量或密钥管理系统中，避免提交到版本库。
- 本地开发优先绑定 `127.0.0.1`；Gateway 绑定 `0.0.0.0` 前必须启用认证。
- 启用 evolution 后建议先以 `auto_promote: false` 运行若干轮，经人工审核确认候选质量后再开启自动晋升。
- 排查问题时优先查看 `echo-agent status` 与 `echo-agent evolution status`；Linux 上可继续查看 `echo-agent service logs`。

---

## 开发

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

uv venv venv --python 3.11
source venv/bin/activate
uv pip install -e ".[all,dev]"

ruff check .
pytest
echo-agent run -w .
```

提交 PR 之前：

- 运行 `ruff check .` 与 `pytest`
- 为行为变更添加或更新测试
- 用户可见变更同步更新文档
- 涉及 evolution、memory、permissions、storage、tool execution 的较大改动建议先开 issue 讨论

---

## 参与贡献

欢迎以下形式的贡献：

- **报告 Bug**：通过 [Issues](https://github.com/fuyuxiang/echo-agent/issues) 提交，附复现步骤与日志
- **新增通道适配器或工具**：参考 `echo_agent/channels/` 与 `echo_agent/agent/tools/` 下的现有实现
- **改进自进化质量**：贡献评测数据集、报告 evolver 的失败案例
- **文档与示例**：补全 `docs/`、添加 `examples/`

详细贡献指南见 `CONTRIBUTING.md`（待补全）。

---

## 许可证

`pyproject.toml` 声明本项目采用 MIT License。

> TODO：仓库根目录暂未提交 `LICENSE` 文件，建议补充。
