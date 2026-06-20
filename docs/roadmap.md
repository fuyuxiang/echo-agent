# Post-1.0 候选

以下能力在 0.3.0 移除，将来带各自设计、以非破坏性加字段方式回归：

| 能力 | 移除的字段 | 回归时需要的设计 |
|---|---|---|
| 推理强度控制 | `agent.reasoning_effort` | 抹平各家 provider reasoning 语义差异的映射层 |
| 会话归档 | `session.archive_after_hours` | 归档器：触发条件、归档目标、可检索性 |
| 可关闭引用 | `knowledge.require_citations` | 引用生成处的开关 + 默认行为决策 |
| 主动推送 | `gateway.platforms{}.home_channel` / `home_chat_id` | 推送触发时机、去重、频控 |
| 多后端存储 | `storage.backend` / `storage.workspace_dir` | filesystem 后端实现 + 后端抽象 |
