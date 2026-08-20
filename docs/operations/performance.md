# 性能优化

Echo Agent 的性能瓶颈通常在模型调用延迟和本地数据 I/O。本文介绍关键调优手段。

---

## 性能瓶颈分析

典型请求的时间分布：

```
┌─────────────────────────────────────────────────────────────┐
│ 模型调用 (60-80%)  │ 工具执行 (10-25%) │ 本地 I/O (5-15%) │
└─────────────────────────────────────────────────────────────┘
```

| 瓶颈 | 表现 | 优化方向 |
|------|------|---------|
| 模型延迟 | 响应等待时间长 | 模型选择、缓存、路由 |
| SQLite I/O | 记忆检索慢 | 索引优化、WAL 模式 |
| 内存占用 | OOM 或 swap | 溢写配置、会话裁剪 |
| 知识库检索 | 相似度搜索慢 | 索引重建、分片 |
| 工具执行 | 外部调用超时 | 超时配置、并行执行 |

---

## 模型调用优化

### 模型选择策略

不同场景选择合适的模型，平衡成本与速度：

按任务类型把不同场景分流到合适的模型，平衡成本与速度。分流规则写在 `models.routes`，用 `task_types` 匹配：

```yaml
models:
  default_model: claude-sonnet-4-5
  routes:
    - task_types: [simple]
      provider: openai
      model: gpt-4o-mini
      max_tokens: 1000
    - task_types: [coding]
      provider: anthropic
      model: claude-sonnet-4-5
      max_tokens: 8000
      fallback_models: [gpt-4o]
```

配置中没有 `models.routing` 这样按场景名分组的嵌套结构，分流一律通过 `routes` 列表的 `task_types` 表达。

### 上下文压缩

Echo Agent 不缓存模型响应（配置中没有 `models.cache`）。降低重复输入开销的手段是上下文压缩，配置在 `compression`：

```yaml
compression:
  enabled: true
  trigger_ratio: 0.7          # 上下文占用达到该比例时触发
  summary_target_ratio: 0.2   # 压缩后目标占比
  tool_pruning_enabled: true  # 裁剪历史工具输出
```

### 并行工具调用

多个独立工具可以并行执行，配置在 `agent.tool_concurrency`，默认已启用：

```yaml
agent:
  tool_concurrency:
    enabled: true
    max_concurrent: 4
```

!!! tip "模型降级"
    配置 fallback 模型，当主模型响应超时时自动切换到更快的备选模型。详见 [模型路由与降级](../guides/models/routing-fallback.md)。

---

## SQLite 优化

Echo Agent 使用 SQLite 存储元数据和记忆。以下配置可显著改善 I/O 性能：

### WAL 模式

SQLite 的连接参数（`journal_mode`、`synchronous`、`cache_size`、`mmap_size` 等 PRAGMA）**不可通过配置调整** —— 配置中没有 `database` 节，代码里也没有设置这些 PRAGMA 的位置。

可配置的只有存储路径，位于 `storage`：

```yaml
storage:
  database_path: data/echo_agent.db
  sessions_dir: data/sessions
  memory_dir: data/memory
  logs_dir: data/logs
  spill_dir: data/spill
```

数据库层面可做的优化因此集中在两点：把 `database_path` 放在本地 SSD 而非网络文件系统上，以及定期做 `VACUUM` 回收碎片。

### 索引维护

```bash
# 检查数据库大小和碎片
sqlite3 ~/.echo-agent/data/echo_agent.db "
  SELECT page_count * page_size as size_bytes FROM pragma_page_count(), pragma_page_size();
"

# 重建索引（离线操作）
sqlite3 ~/.echo-agent/data/echo_agent.db "REINDEX;"

# 回收空间
sqlite3 ~/.echo-agent/data/echo_agent.db "VACUUM;"
```

!!! tip "定期 VACUUM"
    大量删除操作后 SQLite 不会自动收缩文件。建议在低负载时段定期执行 VACUUM。

---

## 内存管理

### 进程内存控制

上下文与历史规模由 `session` 控制（`runtime` 只有 `single_instance` 一个字段，与内存无关）：

```yaml
session:
  context_window_tokens: 0        # 0 表示按模型自身窗口
  max_history_messages: 500       # 会话历史保留条数
  compression_window_cap: 200000  # 压缩窗口上限
  expiry_hours: 72                # 会话过期时间
```

### Spill（溢写）配置

工具输出过大时溢写到磁盘。配置在顶层 `spill` 节，不在 `runtime` 下：

```yaml
spill:
  enabled: true
  max_inline_chars: 6000     # 超过该字符数则溢写
  max_total_mb: 512          # 溢写目录总大小上限
  retention_days: 7          # 保留天数
  sweep_interval_hours: 6    # 清理扫描间隔
```

溢写目录路径由 `storage.spill_dir` 指定。溢写产物可用 `read_spill` 工具按需读回，详见[内置工具参考](../reference/tools.md)。

### 内存监控

```bash
# 查看进程内存使用
echo-agent status

# Linux: 详细内存映射
cat /proc/$(pgrep -f echo-agent)/status | grep -i vm
```

---

## 知识库检索优化

### 向量索引调优

知识库索引是本地 JSON 文件，不是 HNSW 之类的向量索引库，因此没有 `ef_construction`、`ef_search`、`m` 这类调参项 —— 配置中也不存在 `knowledge.index` 嵌套节。可调的是分块与检索规模：

```yaml
knowledge:
  enabled: true
  chunk_size: 1200        # 文档分块大小（默认 1200）
  chunk_overlap: 120      # 分块重叠（默认 120）
  max_results: 5          # 单次检索返回条数
  auto_index: true        # 变更后自动索引
  index_path: data/knowledge_index.json
  docs_dir: data/knowledge
```

分块偏小会增加检索条数与召回噪声，偏大则降低定位精度。调整后需要重建索引才会生效。

### 索引重建

索引是派生物：结构损坏时不要尝试修补，直接重建。重建可用 `knowledge_index` 工具（`action: rebuild`），或在 Dashboard 的 Knowledge 页触发。

嵌入与重排模型的相关参数在 `memory` 节（`embedding_backend`、`rerank_enabled`、`rerank_top_k` 等），详见[配置参考](../reference/configuration.md)。

---

## 网络优化

配置中没有统一的 `network` 节，也没有连接池与全局超时字段。网络相关设置分散在各自的使用方：

| 配置项 | 作用 |
|--------|------|
| `execution.network_policy` | 出站网络总闸，默认 `deny` |
| `tools.web.proxy` | Web 工具的代理地址 |
| `tools.web.timeout_seconds` | Web 工具超时，默认 `30` |
| `tools.browser.nav_timeout_sec` | 浏览器导航超时 |
| `channels.telegram.proxy` | Telegram 通道的代理地址 |
| `gateway.media_download_concurrency` | 网关媒体下载并发，默认 `4` |

```yaml
execution:
  network_policy: allow

tools:
  web:
    enabled: true
    proxy: "http://proxy:8080"
    timeout_seconds: 30
```

所有出站请求都经 `echo_agent/security/net_guard.py` 的统一 SSRF 校验，私有地址默认被拒绝；确需访问内网时才放开对应的 `allow_private_addresses`。

---

## 资源建议

### 最低配置

| 资源 | 最低要求 | 说明 |
|------|---------|------|
| CPU | 2 核 | 工具并行执行需要 |
| 内存 | 2 GB | 基础运行 + SQLite 缓存 |
| 磁盘 | 1 GB | 数据库 + 日志 + 溢写 |
| 网络 | 稳定连接 | 模型 API 调用 |

### 推荐配置

| 资源 | 推荐值 | 说明 |
|------|--------|------|
| CPU | 4 核 | 流畅的工具并行 |
| 内存 | 4-8 GB | 充足的缓存空间 |
| 磁盘 | 10 GB SSD | 快速 I/O |
| 网络 | 低延迟 | 减少模型调用等待 |

---

## 性能诊断命令

```bash
# 查看运行状态和资源使用
echo-agent status

# 查看依赖状态
echo-agent deps status

# 配置验证（检查不合理的参数）
echo-agent config validate

# 成本分析（定位高消耗操作）
echo-agent cost --group-by model
```

!!! note "没有内置的 benchmark 命令"
    不存在 `echo-agent eval --benchmark` 这样的性能基线命令。`echo-agent eval` 用于技能评测，参数为 `--dataset`、`--tag`、`--parallel`、`--output`，衡量的是回答质量而非机器性能。

    需要性能基线时，用 `echo-agent cost` 观察每轮的 token 与耗时，或按[可观测性](observability.md)接入 OpenTelemetry 采集分阶段延迟。
