# 记忆管理

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

长期记忆与会话历史分开保存。运行同一工作区的 Runtime 可以跨会话召回已持久化的事实；清屏不会删除记忆，工作区文件 checkpoint 也不包含记忆备份。

## 四层记忆与存储

| 层级 | 状态所有者与用途 | 生命周期 |
|---|---|---|
| Working | 当前 conversation epoch 的进程内活动上下文 | reset 后清理；不是长期备份 |
| Episodic | SQLite `memory_episodes` 中的会话摘要 | 可按会话检索；重启后保留已落库摘要 |
| Semantic | `storage.memory_dir` 中的 USER/ENVIRONMENT 事实及对应检索索引 | 通过统一 MemoryService 写入、持久化、失效缓存与审计 |
| Archival | 退出活动召回的历史条目 | 供管理/维护检查；不会以低权重继续进入普通 Agent 检索 |

`superseded_by` 非空的旧版本也不参与普通工具、检索或快照；版本关系供管理员追溯，不能把已纠正事实当作最新事实召回。USER 来源优先级可阻止模型推断覆盖用户明确陈述；ENVIRONMENT 写入还受环境记忆准入策略约束。

## 写入、纠错与删除

在已认证会话中，可直接让 Agent 记住、查找或纠正一项事实。模型使用 `memory` 工具，以下是实际参数对象，不是 CLI 命令：

```json
{"action":"add","target":"user","key":"project_db","content":"项目使用 PostgreSQL 15","source":"user_stated","tags":"project,database"}
```

```json
{"action":"search","target":"user","query":"项目数据库"}
```

先查询当前内容，再精确替换：

```json
{"action":"replace","target":"user","old_text":"项目使用 PostgreSQL 15","content":"项目使用 PostgreSQL 16","source":"user_stated"}
```

删除同样先核实条目，再使用原文匹配：

```json
{"action":"remove","target":"user","old_text":"项目使用 PostgreSQL 16"}
```

`list` 列出可见记忆；`list_contradictions` 查看待裁决冲突，`resolve_contradiction` 携带 `contradiction_id` 和 `winner_id` 解决冲突。完整参数见[工具参考](../reference/tools.md#memory)。工具不提供 `memory store/delete/status` 命令，也没有通过 `mode` 参数直接切换底层检索器的契约。

写入必须经过作用域、ENV 门禁、来源优先级与内容扫描，失败会明确返回原因。删除完成后会使检索与快照缓存失效；若权限或来源保护拒绝删除，不应把请求已返回当成删除成功。

## 身份与作用域

会话键管理模型上下文；`memory_scope` 管理长期事实。owner-aware 配置允许同一所有者在明确绑定的入口间复用记忆；群聊遵循既有会话/群组边界。不同 token 不是整个实例的多租户隔离保证，不应自行把用户、群聊和 owner scope 混用。参见[工作区、会话与身份](../concepts/workspace-session-identity.md)。

Dashboard Memory 页供管理员跨主体浏览、筛选、搜索与删除；普通 API 令牌只能访问自身允许的作用域。管理 API 为 `GET {api_prefix}/memory`、`POST {api_prefix}/memory/search`、`PUT /memory/{id}` 与 `DELETE /memory/{id}`，认证与字段以[Gateway API](../reference/gateway-api.md)为准。

## 自动提取、整合与遗忘

ResponseStage 在非评测会话完成回复后检查尚未整合的消息数量，达到阈值再把会话快照交给 DURABLE 后台任务。`sleep_consolidation` 控制该任务中的 Episode、事实提取与反思步骤；它不是一个“空闲若干秒就运行”的独立时钟。整合成功且原历史快照仍匹配后才推进 `last_consolidated`，失败会保留边界供重试。

内容是否值得保存由提取/审核逻辑判断；`eligibility.py` 负责条目在不同读取受众下是否可见，结合 tier、来源和任务状态等规则，并非独立的“值得记忆”评分模型。衰减、访问强化和矛盾裁决按配置与来源执行；固定的归档状态不代表核心事实会自动永存。

`MemoryEntry` 的溯源字段包括 `source`、创建/更新时间以及版本关系；没有可供调用方构造的 `Provenance` 子对象。外部工具输出仍是不可信数据，不能因为写进记忆就变成系统指令。

## 检索与依赖诊断

检索管线结合关键词、向量与可选重排；工作上下文、语义事实和 Episode 的读取边界不同，不能假定所有入口都支持实体/时间过滤。当前 Episode 的 `search_episodes` 接受 query、session_key 和 limit。

本地 embedding 可能需要首次下载模型。远程服务或本地模型不可用时查看日志、依赖状态和记忆统计，确认是否进入关键词降级；降级的相关性不等于向量检索已通过。配置 `memory.enabled`、`embedding_backend`、重排和整合选项见[配置参考](../reference/configuration.md)。

## 重启、备份与验证

1. 写入一个不会暴露敏感信息的测试事实，收到明确成功后停止 Runtime。
2. 使用相同配置和工作区重新启动，在新会话搜索该事实并核对来源/作用域。
3. 替换纠错、再次检索；删除后确认列表、检索和下一回合上下文不再使用旧内容。
4. 备份同时保留 memory JSON、SQLite（含 Episode/版本相关数据）、配置和密钥，不能只备份向量索引。步骤见[备份与恢复](../operations/backup-restore.md)。
