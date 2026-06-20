# Changelog

## [Unreleased]

### 微信通道支持发送文件/图片

修复 weixin 通道出站只发文本、丢弃附件的问题，移植 iLink CDN 加密上传协议。

- `weixin` 通道 `send()` 现按 content block 路由：图片走 `image_item`，文件/音频/视频走 `file_item` 附件，文本不变
- 新增 `send_file` 工具：让 agent 主动将本地文件/图片发送到指定 channel/chat（含工作区路径校验）
- 打通"定时生成 Word 并发送到微信"链路：cron 触发 → 生成 .docx → `send_file` → 微信附件

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
