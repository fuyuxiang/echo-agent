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

```yaml
models:
  routing:
    # 简单对话用小模型
    simple:
      model: gpt-4o-mini
      max_tokens: 1000
    # 复杂推理用大模型
    complex:
      model: claude-sonnet-4-20250514
      max_tokens: 4000
    # 代码生成
    coding:
      model: claude-sonnet-4-20250514
      max_tokens: 8000
```

### 模型响应缓存

对重复或相似请求启用缓存：

```yaml
models:
  cache:
    enabled: true
    ttl_seconds: 3600
    max_entries: 1000
```

### 并行工具调用

当 Agent 需要调用多个独立工具时，启用并行执行：

```yaml
tools:
  parallel_execution: true
  max_concurrent: 4
```

!!! tip "模型降级"
    配置 fallback 模型，当主模型响应超时时自动切换到更快的备选模型。详见 [模型路由与降级](../guides/models/routing-fallback.md)。

---

## SQLite 优化

Echo Agent 使用 SQLite 存储元数据和记忆。以下配置可显著改善 I/O 性能：

### WAL 模式

Write-Ahead Logging 提高并发读写性能：

```yaml
database:
  journal_mode: WAL          # 默认已启用
  synchronous: NORMAL        # FULL / NORMAL / OFF
  cache_size: -64000         # 64MB 页缓存（负数表示 KB）
  mmap_size: 268435456       # 256MB 内存映射
```

### 关键配置说明

| 参数 | 推荐值 | 说明 |
|------|--------|------|
| `journal_mode` | `WAL` | 读写分离，提高并发 |
| `synchronous` | `NORMAL` | 平衡安全与性能 |
| `cache_size` | `-64000` | 页缓存大小 (KB) |
| `mmap_size` | `268435456` | 内存映射大小 (256MB) |
| `busy_timeout` | `5000` | 锁等待超时 (ms) |

!!! warning "synchronous = OFF"
    将 `synchronous` 设为 `OFF` 可获得最佳写入性能，但系统崩溃时可能丢失数据。生产环境建议保持 `NORMAL`。

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

```yaml
runtime:
  memory:
    max_context_tokens: 128000    # 单次上下文最大 token
    session_history_limit: 50     # 会话历史保留轮数
    gc_interval_seconds: 300      # 垃圾回收间隔
```

### Spill（溢写）配置

工具输出超过内存阈值时自动溢写到磁盘：

```yaml
runtime:
  spill:
    enabled: true
    threshold_bytes: 1048576      # 1MB 触发溢写
    directory: ~/.echo-agent/data/spill
    max_disk_usage_bytes: 1073741824   # 1GB 磁盘上限
    cleanup_after_seconds: 86400       # 24 小时后清理
```

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

```yaml
knowledge:
  index:
    type: hnsw                # HNSW 近似最近邻
    ef_construction: 200      # 构建时精度（越高越准，越慢）
    ef_search: 50             # 检索时精度
    m: 16                     # 每节点连接数
  chunk_size: 512             # 文档分块大小
  chunk_overlap: 64           # 分块重叠
```

### 索引重建

知识库数据变更较多后，重建索引可恢复检索性能：

```bash
# 通过 Agent 对话触发或编程接口调用
echo-agent eval --task "重建知识库索引"
```

---

## 网络优化

### 连接池

```yaml
network:
  connection_pool:
    max_connections: 20
    max_keepalive: 10
    keepalive_expiry: 30
  timeout:
    connect: 10
    read: 60
    write: 30
```

### 代理配置

```yaml
network:
  proxy:
    http: "http://proxy:8080"
    https: "http://proxy:8080"
    no_proxy: "localhost,127.0.0.1"
```

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

!!! question "需维护者确认"
    是否提供内置 benchmark 命令（如 `echo-agent eval --benchmark`）用于测量本机性能基线？
