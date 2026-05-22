# Echo Agent

<p align="center">
  <strong>🧬 自进化技能库 × 🧠 认知级记忆系统</strong>
</p>

<p align="center">
  <em>面向私有基础设施的长期运行 Agent 系统 —— 让技能在运行轨迹中持续演化，让记忆在会话之外持续沉淀。</em>
</p>

<p align="center">
  <a href="README.md">中文</a> ·
  <a href="README.en.md">English</a>
</p>

<p align="center">
  <a href="#核心能力">核心能力</a> ·
  <a href="#项目状态">项目状态</a> ·
  <a href="#快速开始">快速开始</a> ·
  <a href="#自进化-self-evolution">自进化</a> ·
  <a href="#认知记忆系统">认知记忆</a> ·
  <a href="#架构">架构</a>
</p>

<p align="center">
  <img alt="Python 3.11+" src="https://img.shields.io/badge/python-3.11%2B-3776AB.svg?logo=python&logoColor=white&style=for-the-badge">
  <img alt="Self Evolving" src="https://img.shields.io/badge/self--evolving-✓-22c55e?style=for-the-badge">
  <img alt="Status Alpha" src="https://img.shields.io/badge/status-alpha-f59e0b.svg?style=for-the-badge">
  <a href="LICENSE"><img alt="License MIT" src="https://img.shields.io/badge/license-MIT-blue.svg?style=for-the-badge"></a>
  <img alt="Self Hosted" src="https://img.shields.io/badge/self--hosted-111827?style=for-the-badge">
</p>

---

## 概览

**Echo Agent** 是一个面向私有基础设施的长期运行 Agent 系统，在传统 Agent 框架之上引入完整的运行时改进闭环。

每一次任务执行被记录为结构化 Trajectory；Evolver 模块以这些 Trajectory 为输入，由 LLM 提议候选技能变更；候选不直接生效，而是先在评测数据集上完成 baseline 与 candidate 的 A/B 对比，仅当指标严格优于 baseline 时晋升进入技能库，否则自动驳回，已晋升的变更支持一键回滚。整个闭环——记录、反思、提议、评测、晋升、冷却——在用户自有的服务器上完成，除模型 API 调用外不向外部发送数据。

除自进化内核外，Echo Agent 还提供：planner / coder / researcher / operator 等多角色协作；由 Working / Episodic / Semantic / Archival 构成的四层记忆系统；针对高风险工具的 LLM 智能审批；原生 A2A 与 MCP 协议支持；覆盖 Telegram、Discord、Slack、微信、QQ、飞书等 12+ 通道的统一消息接入层。

支持 OpenAI、Anthropic Claude、Google Gemini、AWS Bedrock、OpenRouter，以及任何 OpenAI 兼容端点。

---

## 核心能力

Agent 框架的下一个分水岭，不在工具数量、不在模型适配、也不在编排语法，而在两件长期被忽略的事：**能力是否会随运行时间增长**，**记忆是否能跨越会话边界沉淀**。Echo Agent 围绕这两点重建了底层抽象。

<table>
<tr>
<td width="50%" valign="top">

### 🧬 自进化技能库

**让技能库随运行时间持续演化，而非在部署时被冻结。**

传统 Agent 框架的能力边界在部署时即被固定；运行时观测到的失败模式无法反向作用于技能定义。Echo Agent 把每一次任务执行转化为可验证的改进信号，将运行时数据闭环回技能定义本身。

- **轨迹采集**：Agent Loop 中的每一次任务、工具调用、反思评分均被记录为结构化 Trajectory，写入 SQLite 持久层
- **候选生成**：Evolver 模块以失败与低分轨迹为输入，由 LLM 经结构化工具调用提交候选变更（`create` / `patch` / `disable`），每条候选附带可证伪的预期改进指标
- **A/B 评测晋升**：`PromotionGate` 对技能目录做快照后，在隔离副本中应用候选，与 baseline 在评测数据集上对比；指标严格优于 baseline 方可晋升
- **回归守门与冷却**：超过 `regression_threshold` 的候选直接驳回；晋升后默认进入 24 小时冷却期；`evolution rollback <skill>` 一键回滚最近一次晋升
- **完整审计**：Trajectory、SkillCandidate、EvolutionRun 全部持久化，候选可追溯至源轨迹

```bash
echo-agent evolution status         # 引擎状态与待审候选
echo-agent evolution run            # 手动触发一轮 evolution pass
echo-agent evolution rollback <id>  # 回滚最近一次晋升
```

→ [完整设计与配置](#自进化-self-evolution)

</td>
<td width="50%" valign="top">

### 🧠 认知级记忆系统

**让记忆超越上下文窗口，跨越会话边界长期持有。**

单层向量库无法刻画记忆的时序衰减与语义冲突；上下文窗口的硬截断不构成长期记忆。Echo Agent 借鉴认知科学的分级模型，构建了从工作记忆到归档记忆的完整生命周期管理。

- **四层分级架构**：Working / Episodic / Semantic / Archival，覆盖从进程内缓冲到归档冷存的完整生命周期
- **Ebbinghaus 自适应衰减**：`half_life = base × (1 + log₂(1 + access_count))`；访问频次决定半衰期，有效重要性低于阈值时自动降级或清除
- **版本化记忆格的矛盾检测**：新旧记忆的语义冲突不被静默覆盖，作为时序边写入图谱，支持信念修正与历史回溯
- **混合检索（Resonance Scoring）**：BM25 关键词与 FAISS 向量按查询熵自适应加权；Ebbinghaus 衰减因子参与 rerank 阶段
- **Sleep-time 整理管线**：会话结束后由 `MemoryConsolidator` 与 `MemoryReviewer` 完成情节生成、语义事实提取、矛盾检测与归档扫描

> **冲突示例**：用户先前断言"对密集图案存在不适反应"，后续表达"偏好观察密集星空场景"。系统将其识别为信念潜在冲突，建立时序边而非覆盖原记忆，并在后续相关任务的上下文构建中暴露完整的信念变迁链。

→ [完整设计与检索机制](#认知记忆系统)

</td>
</tr>
</table>

### 闭环耦合

```text
认知记忆 ──→ 提供高质量轨迹与上下文 ──→ 自进化产出更准确的候选
   ▲                                              │
   │                                              │
   └────── 进化后的技能产生更优执行路径 ──────────┘
```

记忆系统为进化引擎提供可学习的样本与上下文；进化引擎反向提升轨迹质量与技能库的可用性。两者构成系统的核心反馈回路——这也是 Echo Agent 区别于一次性编排框架的根本所在。

---

## 为什么选择 Echo Agent

除上述核心能力外，Echo Agent 还提供：

- **完全私有部署**：除模型 API 调用外，轨迹、记忆与对话不离开本地；持久化数据落于本地 SQLite 与文件系统。
- **统一消息总线**：CLI、Webhook、计划任务、12+ 消息通道（Telegram / 微信 / 飞书 / Slack 等）与 Gateway API 共享同一 Agent Loop，会话、记忆、技能、权限边界在所有入口一致。
- **细粒度权限模型**：Shell、文件写、代码执行等高风险工具默认进入审批流程，由 LLM 风险评估、路径策略与人工管理员协同决策。
- **多 Agent 协作**：planner、coder、researcher、operator 等专业角色按任务自动路由，支持并行执行与长任务编排。
- **A2A + MCP 原生支持**：实现 Google A2A 协议（可被外部 Agent 发现与调用）；集成 Anthropic MCP（可挂载任意 MCP server 的工具）。

---

## 项目状态

Echo Agent 处于 **alpha** 阶段。配置字段、内部存储 schema 与 API 在稳定版前可能调整。

| 模块 | 状态 | 说明 |
|------|------|------|
| CLI 运行时 | Beta | 交互式命令行与前台运行已可用 |
| 配置与凭证 | Beta | 含交互式 setup 向导与多模型 provider |
| Gateway (REST / WebSocket) | Alpha | 公网部署须启用认证 |
| 自进化 Self-Evolution | Experimental | 生产场景建议 `auto_promote: false`，人工审核晋升 |
| 四层记忆系统 | Experimental | FAISS 为可选依赖，未安装时降级为关键词检索 |
| A2A / MCP 协议 | Experimental | 协议本身仍在演进 |
| 消息通道 | Experimental | 各通道稳定性受第三方 API 与适配器质量影响 |
| 评测框架 | Experimental | 进化质量取决于评测数据集的覆盖度 |

---

## 快速开始

### 环境要求

- Python **3.11+**
- Linux、macOS 或 WSL2
- 至少一个模型 provider 的 API key（OpenAI / Anthropic / Gemini / Bedrock / OpenRouter / 任何 OpenAI 兼容端点）
- 推荐使用 [`uv`](https://docs.astral.sh/uv/) 管理虚拟环境

### 源码安装（推荐）

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

> 一键脚本会写入 `PATH` 并安装 systemd 服务（Linux 环境下）。生产部署请使用源码安装路径，自行管理虚拟环境与服务注册。

### 3 分钟上手

```bash
# 1. 启动交互式 CLI
echo-agent

# 2. 在 CLI 中尝试任意一条任务
> 帮我写一个监控服务器剩余磁盘的 Python 脚本，保存为 disk_check.py

# 3. 观察:
#   - Agent 计划 → 工具调用 → 高风险工具(write_file)进入审批
#   - 任务结束后,本次轨迹自动写入 SQLite 供后续 evolution 使用

# 4. 查看自进化状态
echo-agent evolution status
```

---

## 自进化 Self-Evolution

> 传统 Agent 框架的能力边界在部署时即被固定。Echo Agent 把每一次任务执行转化为可验证的改进信号，使技能库随运行时间持续演化。

### 闭环管线

```text
   ┌──────────────────────┐
   │  TrajectoryRecorder  │  捕获 Agent Loop 中每次任务的完整轨迹
   └──────────┬───────────┘
              │
   ┌──────────▼───────────┐
   │       Evolver        │  LLM 基于轨迹提议候选技能变更
   └──────────┬───────────┘
              │
   ┌──────────▼───────────┐
   │    PromotionGate     │  baseline / candidate A/B 评测
   └──────────┬───────────┘
              │
       ┌──────┴──────┐
       │             │
   ┌───▼────┐   ┌────▼────┐
   │ 晋升   │   │ 驳回    │
   │ + 冷却 │   │ + 还原  │
   └───┬────┘   └─────────┘
       │
       └─→ 反馈至下一轮任务执行
```

### 关键属性

- **永远先评测后生效**：候选不会直接覆盖技能库；先对技能目录做快照，在隔离副本中应用候选，再分别在 baseline 与 candidate 上跑评测。
- **回归阈值守门**：评测指标退步超过 `regression_threshold` 的候选直接驳回；`require_strict_improvement` 开启时与 baseline 持平亦视为不通过。
- **冷却期**：技能晋升后默认进入 24 小时冷却期，避免短期内重复变更。
- **一键回滚**：`echo-agent evolution rollback <skill>` 还原最近一次晋升。
- **完整审计**：Trajectory、SkillCandidate、EvolutionRun 均持久化，候选可追溯至源轨迹。
- **人工档**：关闭 `auto_promote` 后，候选进入 `needs_review`，需 `evolution promote <id>` 显式晋升。

### 启用配置（生产建议默认）

在 `echo-agent.yaml` 中加入以下配置即可启用进化引擎；生产部署建议先以 `auto_promote: false` 运行若干轮，确认候选质量后再切换。

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
  auto_promote: false                  # 生产建议先关闭,人工审核
  candidate_review_required: true
  cooldown_seconds_after_promote: 86400
  trajectory_retention_days: 30
  skill_size_limit_bytes: 50000
  redact_args: true
```

### CLI 一览

```bash
echo-agent evolution status              # 引擎状态、待审候选、最近一次 run
echo-agent evolution run                 # 手动触发一次完整 evolution pass
echo-agent evolution list-candidates     # 列出候选(--status pending|promoted|rejected|needs_review)
echo-agent evolution show-candidate <id> # 候选详情:rationale、预期改进、A/B 报告
echo-agent evolution promote <id>        # 手动晋升一个 needs_review 候选
echo-agent evolution rollback <skill>    # 回滚某技能的最近一次晋升
echo-agent evolution init-dataset        # 初始化 baseline 评测数据集
```

候选格式、评分细节与回滚行为见 `docs/evolution.md`。

---

## 配置

Echo Agent 按以下优先级加载配置：`-c` 参数指定的文件 > 工作区中的 `echo-agent.yaml` > `~/.echo-agent/echo-agent.yaml`。

最小可用配置（推荐使用环境变量传入凭证）：

```yaml
workspace: "~/.echo-agent"

models:
  defaultModel: "gpt-4o-mini"
  providers:
    - name: "openai"
      apiKeyEnv: "OPENAI_API_KEY"      # 从环境变量读取,避免明文落库

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

> **避免将 API key 直接写入 `echo-agent.yaml`**。建议使用环境变量、本地 only 的配置覆盖文件，或外部密钥管理系统（Vault、AWS Secrets Manager 等）。

支持的 provider 包括 `openai`、`anthropic`、`gemini` / `google`、`bedrock` / `aws`、`openrouter`，以及任何 OpenAI 兼容端点。模型路由支持按任务类型匹配、fallback 策略与凭证池轮换。

环境变量覆盖采用 `ECHO_AGENT_` 前缀，层级之间以双下划线分隔，例如 `ECHO_AGENT_GATEWAY__PORT=9000`。

---

## 常用命令

```bash
echo-agent                    # 启动交互式命令行
echo-agent run                # 前台运行 Agent
echo-agent setup              # 完整配置向导(含 evolution 子向导)
echo-agent setup model        # 配置模型与 provider
echo-agent setup channel      # 配置消息通道
echo-agent status             # 查看当前配置与运行状态
echo-agent gateway            # 启动 Gateway 服务
echo-agent eval -d eval.yaml  # 运行评测数据集
```

服务管理（仅 Linux）：

```bash
echo-agent service install    # 注册 systemd 服务
echo-agent service start
echo-agent service status
echo-agent service logs
echo-agent service uninstall
```

---

## Gateway API

Gateway 为 Echo Agent 提供 HTTP / WebSocket 接口，适用于自定义前端、内部系统、自动化脚本与外部 Agent 集成。根路径 `/` 提供内置 Playground，便于本地调试。

```bash
echo-agent gateway --host 127.0.0.1 --port 9000
```

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/` | 内置 Playground |
| `GET` | `/api/v1/health` | 健康检查 |
| `POST` | `/api/v1/message` | 发送消息至 Agent |
| `GET` | `/api/v1/sessions` | 查看会话列表 |
| `DELETE` | `/api/v1/sessions/{key}` | 重置 Gateway 会话 |
| `POST` | `/api/v1/pair` | 生成配对码 |
| `POST` | `/api/v1/pair/verify` | 验证配对码 |
| `GET` | `/api/v1/stats` | Gateway 运行统计 |
| `GET` | `/ws` | WebSocket 接口 |
| `GET` | `/.well-known/agent.json` | A2A Agent Card |
| `POST` | `/a2a` | A2A JSON-RPC 入口 |

认证支持 `open`、`allowlist`、`pairing` 三种模式，API token 可通过 `X-Echo-Agent-Token` 或 `Authorization: Bearer` 头部传入。**公网部署必须启用认证与网络层访问控制**。

---

## 通道

所有通道的输入均规范化为统一的消息事件，进入同一消息总线与 Agent Loop。CLI、Telegram、微信、QQBot、Gateway 等不同来源的请求共享一致的会话、记忆、工具与权限边界，并使用同一套持续演化的技能库。

| 分类 | 通道 |
|------|------|
| 本地与系统 | `cli`、`webhook`、`cron` |
| 国际平台 | `telegram`、`discord`、`slack`、`whatsapp`、`email`、`matrix` |
| 国内生态 | `wechat`、`weixin`、`qqbot`、`feishu`、`dingtalk`、`wecom` |

各通道稳定性受第三方 API 政策与适配器实现差异影响，详见 [限制](#限制)。

---

## 认知记忆系统

Echo Agent 的记忆系统在两类记忆（用户记忆与环境记忆）之上实现完整生命周期管理，采用四层分级架构对短期到长期的记忆进行差异化存储与检索。设计目标是在有限存储与有限上下文窗口下，对用户偏好、领域事实与历史经验提供时序敏感、语义可验证的持久化能力。

### 记忆分类

| 类型 | 说明 |
|------|------|
| 用户记忆 (user) | 偏好、习惯、沟通风格、个人上下文。按会话隔离，带 `global` 标签时跨会话可见 |
| 环境记忆 (environment) | 项目事实、工具配置、流程规则、领域知识。全局可见，不受会话隔离约束 |

### 四层记忆层级

| 层级 | 说明 |
|------|------|
| Working | 当前对话的进程内缓冲区，容量受限（默认 20 条），不持久化 |
| Episodic | 对话片段的摘要记录，按会话与时间索引，持久化至 SQLite |
| Semantic | 从情节中提炼的核心事实，作为主要的持久化记忆层，支持 CRUD、关键词与向量检索 |
| Archival | 重要性衰减至阈值以下的记忆自动归档，进一步衰减后清除 |

### 检索：BM25 与向量混合

`HybridRetriever` 融合 BM25 关键词匹配与 FAISS 向量相似度，按查询熵自适应调整两者权重（Resonance Scoring）：模糊查询偏向向量召回，精确查询偏向关键词。Ebbinghaus 衰减因子作为权重参与重排（rerank）阶段；当有效分值低于阈值时，由后台常驻任务触发物理归档或清除。

向量索引基于 FAISS（可选依赖），使用 SQLite 持久化 embedding；未安装 FAISS 时自动降级为纯关键词检索。

### 遗忘曲线

自适应衰减遵循 Ebbinghaus 公式：`half_life = base × (1 + log₂(1 + access_count))`。访问次数越多，半衰期越长，遗忘越慢。有效重要性低于归档阈值时降级至 Archival 层；低于遗忘阈值时彻底清除。

### 矛盾检测：时序边与信念修正

新记忆写入时，通过版本化记忆格（versioned memory lattice）检测与已有记忆的矛盾，支持 LLM 语义验证与启发式（同 key 不同内容）两种模式。**矛盾不会被静默覆盖，而是作为时序边存储**，支持信念修正与历史查询。

> 例如：用户上周说"我有密集恐惧症"，今天说"我喜欢看密密麻麻的星空"。系统会将其识别为潜在冲突，在记忆图谱中建立时序关联（而非直接覆盖旧记忆），并在后续相关任务中向 Agent 暴露完整的信念变迁历史。

### 整理与审查

会话结束后，`MemoryConsolidator` 通过 LLM 将对话摘要写入 `HISTORY.md`，并更新长期记忆 `MEMORY.md`。完整 sleep-time 整理依次执行：创建情节 → 提取语义事实并提升 → 矛盾检测 → 遗忘与归档扫描。

`MemoryReviewer` 在非平凡对话后自动运行，由 LLM 判定是否需要持久化用户偏好、项目事实或经验教训，并执行 add / replace / remove 操作。

### 安全

所有写入记忆的内容经过注入扫描（prompt injection、角色劫持、凭证外泄等模式）与不可见 Unicode 字符检测；文件写入采用原子替换与跨平台文件锁，避免并发写入导致的数据损坏。

---

## 工具与权限

30+ 内置工具按类别组织，由权限与审批系统统一管控；MCP server 可按配置动态注册外部工具。

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
| 多 Agent | `agents_list`、`agents_route` |
| MCP | 由配置中的 MCP server 动态注册 |

高风险工具（如 `exec`、`write_file`、`edit_file`）默认进入审批流程，可通过 `permissions.adminUsers` 与 `permissions.approval` 调整访问控制与审批策略。审批支持 LLM 风险评估（Smart Mode）、路径策略与人工管理员协同。

---

## 技能

技能采用目录与 `SKILL.md` 的开放格式，内置 `arxiv`、`weather`、`summarize`、`plan`、`skill-creator` 等。支持查看、创建、修改、删除，亦可从本地路径、Git 仓库或 URL 安装。

技能库支持运行时自动演化，详见 [自进化](#自进化-self-evolution)。

---

## 架构

请求从 CLI、Gateway、调度器、Webhook 或某个通道适配器进入，被规范化为统一的消息事件，经消息总线路由至 Agent Loop；执行过程经过模型路由、记忆检索、权限审批、工具调用与可观测性采集，最终落入轨迹记录器供进化引擎消费。

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

### 代码导航

```text
echo_agent/
├── a2a/            # A2A 协议(Agent-to-Agent 互操作)
├── agent/          # Agent loop、上下文构建、压缩、工具执行
├── bus/            # 消息事件队列
├── channels/       # CLI、消息通道、webhook、cron 适配器
├── cli/            # 配置向导、状态查看、服务管理、evolution 子命令
├── config/         # 配置 schema、加载器、默认值
├── evaluation/     # 评测数据集与 runner(供 baseline / candidate A/B 使用)
├── evolution/      # 自进化:轨迹记录、evolver、晋升 gate、调度器
├── gateway/        # HTTP / WebSocket Gateway
├── knowledge/      # 知识库索引与检索
├── mcp/            # MCP 客户端、传输层、OAuth
├── memory/         # 四层记忆、混合检索、遗忘曲线、矛盾检测、向量索引
├── models/         # Provider、路由、凭证池
├── observability/  # 健康检查、Span、遥测
├── permissions/    # 权限与凭证原语
├── plugins/        # 插件 hook 系统,自进化模块由此接入 Agent Loop
├── scheduler/      # 计划任务服务
├── security/       # 风险分类、路径策略、LLM 安全审批
├── session/        # 会话持久化
├── skills/         # 技能存储与审查
├── storage/        # SQLite 后端
└── tasks/          # 任务管理与工作流引擎
```

---

## 限制

Echo Agent 当前为 alpha 软件，使用前请了解以下边界：

- **自进化质量取决于评测数据集的覆盖度与代表性**。空数据集或样本过少时，A/B 对比将失去意义。
- **LLM 提议的候选技能可能是错误的**，生产部署建议关闭 `auto_promote`，由人工审核后晋升。
- **记忆抽取可能产生过时或不准确的事实**，关键决策应回到原始上下文核对。
- **Shell、文件编辑、进程控制与代码执行是高权限工具**，仅向受信用户开放；公网入口必须启用认证。
- **Gateway 公网部署须启用 `allowlist` 或 `pairing` 认证**并配合网络层访问控制（防火墙 / 反向代理）。
- **通道适配器依赖第三方 API**，部分实现可能受限于 bot 政策或非官方协议，稳定性不在项目可控范围内。
- **内部存储 schema 与配置字段在稳定版前可能调整**，升级时请阅读 release notes。

---

## 安全建议

- 将 API key、token 与 `data/credentials.json` 存放于环境变量或密钥管理系统中，避免提交到版本库。
- 本地开发优先绑定 `127.0.0.1`；Gateway 绑定 `0.0.0.0` 前必须启用认证。
- Shell、进程与代码执行属于高权限能力，仅向可信用户开放。
- 启用 evolution 后建议先以 `auto_promote: false` 运行若干轮，经人工审核确认候选质量后再开启自动晋升。
- 排查问题时优先查看 `echo-agent status` 与 `echo-agent evolution status`；生产环境可继续查看 `echo-agent service logs`。

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

- **报告 Bug**：通过 [Issues](https://github.com/fuyuxiang/echo-agent/issues) 提交，附带复现步骤与日志。
- **新增通道适配器或工具**：参考 `echo_agent/channels/` 与 `echo_agent/agent/tools/` 下的现有实现。
- **改进自进化质量**：贡献评测数据集、报告 evolver 的失败案例。
- **文档与示例**：补全 `docs/`、添加 `examples/`。

详细贡献指南见 `CONTRIBUTING.md`（待补全）。

---

## License

MIT
