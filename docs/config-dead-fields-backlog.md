# 配置死字段处置 backlog(自动生成,请勿手改)

## fix —— 该接线的功能/真 bug(子项目 C 处理,安全相关走快车道)

| 字段(snake) | reason |
|---|---|
| `channels.wecom.encoding_aes_key` | 企业微信加密回调密钥未接线,wecom.py 仅用明文 token 做 SHA1,从不 AES 解密 |
| `models.providers[].max_retries` | 重试硬编码在 LLMProvider._RETRY_DELAYS,该配置无效果 |
| `tools.exec.timeout_seconds` | shell/process 工具用类级默认值,该字段无效;仅 code_exec.timeout_seconds 生效 |
| `tools.mcp_servers{}.transport` | _create_transport 按 url/command 隐式选择,显式 transport 被忽略 |
| `session.archive_after_hours` | 构造 SessionManager 时未传,代码用默认 168 |
| `memory.hybrid_retrieval` | HybridRetriever 在 loop.py:414 无条件构造,开关不控制任何分支 |
| `memory.adaptive_forgetting` | 遗忘曲线在 store.py:172 无条件创建,开关不生效 |
| `memory.archival_threshold` | store.py:174 硬编码 0.05,配置值未传入 ForgettingCurve |
| `memory.forget_threshold` | store.py:175 硬编码 0.01,配置值未传入 |
| `knowledge.require_citations` | 引用始终生成(index.py 无条件输出 citation),开关不生效 |
| `storage.backend` | app.py:71 永远构造 SQLiteBackend,filesystem 后端不存在,此开关无效 |
| `observability.trace_enabled` | 仅向导提示用,运行时 TraceLogger 在 loop.py 无条件构造,开关不生效 |
| `gateway.platforms{}.enabled` | server.py:90 平台循环只读 rate_limit_rpm,enabled 从不检查,禁用平台无效 |
| `gateway.enable_progressive_edit` | ProgressiveEditor 在 server.py:85 无条件实例化,此开关从不被读;真正开关是 emit_progress_events |
| `planning.max_branches` | 未传入 planner 构造(loop.py 只接 default_strategy/max_tree_depth/reflection_enabled) |
| `a2a.capabilities` | AgentCard 构造时未用,改用 a2a/models.py 默认值 |
| `evaluation.enabled` | 无读取点,eval 子命令被调用时无条件运行 |
| `evaluation.parallel_cases` | 并发度取自 CLI --parallel(默认 3),从不读 config |
| `agent.reasoning_effort` | 仅 schema 定义,从未接线到 provider 的 ChatRequest.reasoning_effort |

## remove —— 纯孤儿字段,建议删除

| 字段(snake) | reason |
|---|---|
| `models.routes[].context_window` | 仅透传进 RouteDecision,无消费方;真实窗口来自 session.context_window_tokens |
| `models.cost_limit_daily_usd` | 无读取点,成本限制由 cost.daily_budget_usd 实现,此字段为误导孤儿 |
| `memory.max_episodes` | 全仓无引用,episode 无数量上限控制 |
| `memory.embedding_batch_size` | 全仓无引用 |
| `memory.consolidation_idle_seconds` | 全仓无引用 |
| `multi_agent.worker_profiles[].provider` | executor 始终用注入的 provider,profile.provider 从不被读 |
| `scheduler.dead_task_timeout_seconds` | 仅 schema 定义,无消费方 |
| `storage.workspace_dir` | 无读取点,agent 直接用 self.workspace/"data" |
| `observability.show_tool_calls` | 无运行时读取点 |
| `observability.show_route_decisions` | 无读取点,路由决策在 inference_stage 无条件记录 |
| `skills.auto_load` | 仅 schema 定义,无消费方 |
| `skills.platform_disabled` | 仅 schema 定义,无消费方 |
| `gateway.platforms{}.home_channel` | 全仓无读取点 |
| `gateway.platforms{}.home_chat_id` | 全仓无读取点 |
| `gateway.platforms{}.reply_mode` | 全仓无读取点 |
| `gateway.max_agent_cache_size` | 仅 schema 定义,无读取点 |

