# Changelog

## [0.3.0] - 2026-06-20

### 配置契约清算（破坏性变更）

收敛配置 schema 至"零死字段"：每个配置项要么真生效，要么移除。

**接线生效（原先配了不生效，现已生效）：**
- `memory.archival_threshold` / `memory.forget_threshold` — 记忆归档/遗忘阈值
- `models.providers[].max_retries` — provider 重试次数
- `observability.trace_enabled` — 执行轨迹开关
- `planning.max_branches` — ToT 分支数
- `a2a.capabilities` — AgentCard 能力声明
- `channels.wecom.encoding_aes_key` — 企业微信加密回调（安全修复）

**移除（虚假能力/冗余/孤儿，配置中请删除以下项）：**
- `storage.backend`、`storage.workspace_dir`（filesystem 后端不存在）
- `tools.exec.timeout_seconds`（请用 `tools.code_exec.timeout_seconds`）
- `tools.mcp_servers{}.transport`（按 url/command 自动选择）
- `gateway.enable_progressive_edit`（请用 `gateway.emit_progress_events`）
- `gateway.platforms{}.enabled` / `home_channel` / `home_chat_id` / `reply_mode`
- `gateway.max_agent_cache_size`
- `memory.hybrid_retrieval`、`memory.adaptive_forgetting`、`memory.max_episodes`、`memory.embedding_batch_size`、`memory.consolidation_idle_seconds`
- `models.cost_limit_daily_usd`（请用 `cost.daily_budget_usd`）
- `models.routes[].context_window`（实际由 `session.context_window_tokens` 驱动）
- `multi_agent.worker_profiles[].provider`
- `evaluation.enabled`、`evaluation.parallel_cases`（并发度用 CLI `--parallel`）
- `scheduler.dead_task_timeout_seconds`
- `observability.show_tool_calls`、`observability.show_route_decisions`
- `skills.auto_load`、`skills.platform_disabled`
- `agent.reasoning_effort`、`session.archive_after_hours`、`knowledge.require_citations`（转入 post-1.0 候选）
