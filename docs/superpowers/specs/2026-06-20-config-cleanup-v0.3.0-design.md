# 配置契约清算 → 0.3.0 → 1.0.0 发版设计

- 日期：2026-06-20
- 版本节奏：当前 `0.2.3` → 清算版 `0.3.0` →（稳定后）`1.0.0`
- 状态：设计待评审

## 1. 背景与目标

Echo Agent 当前版本 `0.2.3`，分类器为 `Development Status :: 3 - Alpha`。目标是发布一个**名副其实的 1.0.0 稳定版**。

按语义化版本（SemVer），1.0.0 的核心承诺只有一条：**公开契约从此稳定，破坏性变更必须走大版本号。** 对一个配置驱动的 agent runtime，公开契约约等于 **配置 schema**。因此 1.0 的硬性质量门是：

> 用户能在 `echo-agent.yaml` 里写的每一个字段，要么真生效，要么压根不存在。"配了被静默忽略" 是稳定版的头号原罪——它比缺功能更糟，因为它欺骗用户。

当前 schema 中存在 **35 个死字段**（`status="dead"`）：定义了却不被运行时读取，或被硬编码值架空。这是 1.0 的直接阻塞项。

**本设计的目标**：通过一个清算版 `0.3.0`，把配置 schema 收敛到"零死字段"——每个字段都名副其实——然后将同一份代码贴 `1.0.0` 标签。

**非目标**：
- 不新增功能特性（清算只做"接线已有功能"或"移除空声明"）。
- 不处理 `agent.reasoning_effort` 等需要独立设计的能力（见 §6 post-1.0 候选）。
- 不做与本目标无关的重构。

## 2. 死字段成因（git 考证结论）

对 35 个字段逐一追溯 git 历史后，成因明确——**绝大多数是"从未接线"，而非"中途删了实现"**（下表数量为近似分类，三类合计 35）：

| 成因 | 数量 | 说明 | 代表来源 commit |
|---|---|---|---|
| **迁移搬入的空 schema** | ~20 | 早期从 fubot 项目整体搬入 schema，对应实现未迁移/未实现，引入即无消费方 | `312c1fa` |
| **设计先行、实现没跟上** | ~13 | 多 agent / 记忆系统重构时新增 schema，功能未完成 | `edcb010` |
| **真·迁移残留**（曾有实现后被删） | 2 | 重构后改用别的字段/机制，旧字段成孤儿 | 见下 |

2 个真正的迁移残留（曾被运行时代码读取，后被重构删除）：
- `models.cost_limit_daily_usd` —— 早期 router.py 读它做成本限制，`7bf9480` 重构安全层时删除；成本限制改由 `cost.daily_budget_usd` 实现。
- `multi_agent.worker_profiles[].provider` —— 旧 `_select_provider(profile)` 曾按它选 provider，`cdac9c6` 把执行收敛进 runtime 后改用注入的 provider。

> `evaluation.parallel_cases` 形似迁移残留，但配置项本身从未被读取（并发度一直取自 CLI `--parallel`），故归入"冗余"而非迁移残留。

**结论**：没有任何字段属于"实现还在、只是 schema 名字写错对不上"。因此清算动作只有两种——**接线（fix）** 或 **移除（remove）**。

## 3. 处置原则

逐字段二选一，判据是 **"1.0 要不要承诺这个能力"**，而不是"接线难不难"：

- **fix（接线）**：功能已经在运行、只是配置没接进去（硬编码/默认值），或用户合理预期它生效、不生效会造成安全或行为误判。
- **remove（移除）**：虚假能力（实现根本不存在）、与现有字段冗余、或纯孤儿。移除让用户根本配不了，从而消除"配了不生效"。

**重要：remove ≠ 能力永久消失。** 被移除且仍有产品价值的能力，记入 §6 post-1.0 候选清单，将来带各自的设计、以**非破坏性加字段**的方式回归。1.0 的美德是小而稳，不是大而全。

## 4. fix 清单（7 个，0.3.0 必做）

这些字段对应的功能都已在运行，接线成本低（接个值 / 加个守卫），且包含 1 个安全项。

| 字段（snake） | 接线动作 | 成本 | 备注 |
|---|---|---|---|
| `channels.wecom.encoding_aes_key` | 实现企业微信加密回调的 AES 解密；当前仅明文 token 做 SHA1 | 中 | **安全项**，优先级最高 |
| `memory.archival_threshold` | `store.py:174` 把硬编码 0.05 改为从 config 取值，传入 ForgettingCurve | 低 | |
| `memory.forget_threshold` | `store.py:175` 把硬编码 0.01 改为从 config 取值 | 低 | |
| `models.providers[].max_retries` | provider 重试把类级 `_RETRY_DELAYS` 改为按 `max_retries` 生成 | 低 | |
| `observability.trace_enabled` | TraceLogger 构造处加 `if config.observability.trace_enabled` 守卫；CLI 向导已在收集该值 | 低 | |
| `planning.max_branches` | ToT 分支数 `strategies.py:133` 把硬编码 `range(3)` 改为读配置 | 低 | |
| `a2a.capabilities` | `server.py:212` 构造 AgentCard 时补传 capabilities | 低 | |

> 注：`agent.reasoning_effort` 名义上属 fix（provider 有同名字段），但接线要打穿各家 provider 不一致的 reasoning 语义，是可独立成项目的活。**本版不 fix，移除字段，记入 post-1.0 候选**，避免拖住发版。故 fix 清单实际为 7 个（上表）。

## 5. remove 清单（28 个，0.3.0 删除）

35 个死字段中，扣除 7 个 fix，其余 **28 个全部 remove**——直接从 `schema.py` 删除字段定义。包含三类：

**5.1 虚假能力 / 冗余（实现不存在或与现有字段重复）**
- `storage.backend` —— filesystem 后端从不存在，最具误导性，必删
- `tools.exec.timeout_seconds` —— 与生效的 `tools.code_exec.timeout_seconds` 重复
- `tools.mcp_servers{}.transport` —— `_create_transport` 已按 url/command 隐式正确选择
- `gateway.enable_progressive_edit` —— 真正开关是 `emit_progress_events`，重复
- `memory.hybrid_retrieval` / `memory.adaptive_forgetting` —— 核心机制本就常开，"关闭开关"无实现也无意义
- `evaluation.enabled` —— 是否评测由是否执行 eval 子命令决定
- `evaluation.parallel_cases` —— 与 CLI `--parallel` 重复且 CLI 优先
- `gateway.platforms{}.enabled` —— 平台是否启用由是否在 `platforms{}` 中配置决定

**5.2 真·迁移残留（曾有实现，已被别的机制取代）**
- `models.cost_limit_daily_usd` —— 已由 `cost.daily_budget_usd` 取代
- `multi_agent.worker_profiles[].provider` —— 执行已收敛进 runtime，用注入的 provider

**5.3 纯孤儿（schema-only，从未有消费方）**
- `models.routes[].context_window`、`memory.max_episodes`、`memory.embedding_batch_size`、`memory.consolidation_idle_seconds`、`scheduler.dead_task_timeout_seconds`、`storage.workspace_dir`、`observability.show_tool_calls`、`observability.show_route_decisions`、`skills.auto_load`、`skills.platform_disabled`、`gateway.max_agent_cache_size`、`gateway.platforms{}.home_chat_id`、`gateway.platforms{}.reply_mode`

**5.4 原 fix 类中降级 remove 的能力项——本版 remove，能力转入 post-1.0 候选**

这 4 项背后的功能本身没做完或需独立设计，"接线"成本远大于"接个值"。在稳定版塞半成品比"配了不生效"更隐蔽，故移除并登记：
- `agent.reasoning_effort` —— 接线需打穿各家 provider 不一致的 reasoning 语义，可独立成项目
- `session.archive_after_hours` —— 会话归档器尚不存在
- `knowledge.require_citations` —— "可关引用"是未定的产品决策（当前引用无条件生成）
- `gateway.platforms{}.home_channel` —— 主动推送是完整特性（触发时机/去重/频控），非接个频道 ID 即可

## 6. post-1.0 候选清单（被移除但保留想法）

以下能力本版移除，记入 backlog 供 1.x 按需、带各自设计、以**非破坏性加字段**方式回归：

| 能力 | 移除的字段 | 回归时需要的设计 |
|---|---|---|
| 推理强度控制 | `agent.reasoning_effort` | 抹平各家 provider reasoning 语义差异的映射层 |
| 会话归档 | `session.archive_after_hours` | 归档器：触发条件、归档目标、可检索性 |
| 可关闭引用 | `knowledge.require_citations` | 引用生成处的开关 + 默认行为决策 |
| 主动推送 | `gateway.platforms{}.home_channel` / `home_chat_id` | 推送触发时机、去重、频控 |
| 多后端存储 | `storage.backend` / `storage.workspace_dir` | filesystem 后端实现 + 后端抽象 |

> 落地方式建议：在 `backlog`/roadmap 文档里专开一节 "post-1.0 candidates" 记录上表。

## 7. 实现机制

死字段经由 schema 字段的 `json_schema_extra` 元数据驱动文档生成，清算要顺着这套机制走。

**现状机制**（已存在，无需新建）：
- `echo_agent/config/schema.py`：每个字段用 `json_schema_extra` 标注 `status`（`"dead"` / `"effective"`）、`disposition`（`"fix"` / `"remove"` / `"keep"`）、`reason`，有效字段另带 `ref`（指向消费代码位置）。
- `echo_agent/config/docgen.py`：`render_backlog()` 按 `status=="dead"` 聚合，`render_markdown()` / `render_yaml()` 生成 config-reference。
- `echo_agent/cli/config_cmd.py`：`config gen-docs` 把 reference 与 backlog 写入 `docs/`。
- `tests/test_config_backlog.py`：断言 backlog 分组与已知死字段存在。

**清算动作**：

1. **fix 字段（7 个）**：先改实现接线（见 §4 各字段动作），再把该字段 `json_schema_extra` 里的 `status` 从 `"dead"` 改为 `"effective"`，把 `disposition`/`reason` 替换为 `ref`（指向新接线的代码位置）+ `desc_zh`/`desc_en`。

2. **remove 字段（28 个）**：直接从 `schema.py` 删除字段定义。注意嵌套：`gateway.platforms{}`、`models.providers[]`、`models.routes[]`、`multi_agent.worker_profiles[]`、`tools.mcp_servers{}` 是子模型字段，删字段而非删整个模型。

3. **重生文档**：`echo-agent config gen-docs`，重新生成 `config-reference.{md,en.md,yaml,en.yaml}` 与 `config-dead-fields-backlog.md`。清算彻底后，backlog 的 fix/remove 两节应为空。

4. **更新测试**：`tests/test_config_backlog.py` 中断言"已知死字段存在"的用例需要改写——清算后这些字段要么 effective 要么不存在。新断言应校验"backlog 不再包含已清理字段"。fix 字段还需补/改各自的功能回归测试（尤其 wecom 加密解密、两个 memory threshold 生效、max_retries 生效）。

5. **迁移说明**：被删字段对用户是破坏性变更（0.x 允许 break，但须告知）。在 CHANGELOG / 升级说明里列出 28 个被移除字段及替代项（如 `cost_limit_daily_usd` → `cost.daily_budget_usd`）。

## 8. 发版流程

```
0.2.3 (现状)
  │
  ├─ 0.3.0 清算版
  │    • 7 个 fix 接线 + status 改 effective
  │    • 28 个 remove 删除
  │    • gen-docs 重生，backlog 清空
  │    • 测试更新（含 fix 功能回归）
  │    • CHANGELOG + 迁移说明（破坏性：移除 28 字段）
  │    • 分类器可先调至 Development Status :: 4 - Beta
  │    • 同步更新中英文 README / config-reference
  │
  ├─ 稳定观察期（跑一阵，确认无回归）
  │
  └─ 1.0.0 正式版
       • 同一份代码贴 1.0.0 标签（不再做破坏性改动）
       • pyproject.toml version → 1.0.0
       • 分类器 Development Status :: 3 - Alpha → 5 - Production/Stable
       • 发版说明：1.0 是"已稳定之物的正式命名"
```

**为什么先 0.3.0 再 1.0.0**：清算本身是破坏性的（移除 28 字段）。在 0.x 里完成所有 break，让 1.0 成为"已经稳定的东西的正式命名"，而非"希望它稳定"。1.0 一旦发出即承诺配置契约不再随意破坏。

## 9. 验收标准

- [ ] `echo-agent config gen-docs` 后，`config-dead-fields-backlog.md` 的 fix / remove 两节为空
- [ ] `schema.py` 中无任何 `status="dead"` 字段
- [ ] 7 个 fix 字段各有功能回归测试，证明配置值真正生效
- [ ] wecom 加密回调能正确 AES 解密（安全项专项验证）
- [ ] `ruff check .` 与 `pytest` 全绿（CI 同款检查）
- [ ] CHANGELOG 列出全部 28 个被移除字段及替代项
- [ ] 中英文 README 与 config-reference 同步更新
- [ ] post-1.0 候选清单已记入 backlog/roadmap 文档

## 10. 范围与风险

**明确不在本版范围**：reasoning_effort 接线、会话归档、可关引用、主动推送、多后端存储（均见 §6）。

**风险**：
- 移除字段是破坏性变更——靠"0.x 阶段 + 迁移说明"控制，1.0 前完成。
- wecom 加密解密是唯一中等复杂度的 fix，且是安全项，需专项测试，不可仅"接个值"了事。
- `test_config_backlog.py` 是清算的守护测试，改它时要确保新断言真正校验"已清理"，而非放宽成永远通过。

