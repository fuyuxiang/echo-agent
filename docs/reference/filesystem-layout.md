# 文件系统布局

Echo Agent 的数据存储分为全局目录和工作区目录两个层次。

## 目录结构总览

```
~/.echo-agent/                    # 全局数据目录
├── config.yaml                   # 全局配置文件
├── credentials.yaml              # 凭据存储（加密）
├── data/
│   ├── echo_agent.db            # 主 SQLite 数据库
│   ├── memory/
│   │   ├── long_term.json       # 长期记忆
│   │   └── embeddings/          # 记忆向量索引
│   ├── knowledge/
│   │   ├── documents/           # 知识库原始文档
│   │   ├── chunks/              # 分块后的文档片段
│   │   └── index/               # 向量检索索引
│   ├── spill/                   # 大文件溢出存储
│   ├── logs/
│   │   ├── tool_audit.jsonl     # 工具调用审计
│   │   └── memory_audit.jsonl   # 记忆读写审计
│   └── checkpoints/
│       └── chk_<timestamp>/     # 检查点快照
├── skills/
│   ├── installed/               # 已安装的技能
│   ├── staged/                  # 待审批的技能
│   └── evolved/                 # 进化产生的技能
├── plugins/
│   └── <plugin_name>/           # 插件目录
├── cache/
│   ├── models/                  # 模型响应缓存
│   └── web/                     # 网页抓取缓存
└── services/
    ├── gateway.pid              # Gateway 进程 PID
    └── gateway.sock             # Unix Socket（Linux/macOS）

.echo-agent/                      # 工作区数据目录（项目级）
├── config.yaml                   # 工作区配置覆盖
├── memory/
│   └── workspace_memory.json    # 工作区级记忆
├── knowledge/
│   └── local_docs/              # 项目本地知识库
├── tasks/
│   └── active.json              # 当前活跃任务列表
└── sessions/
    └── <session_id>.json        # 会话历史
```

---

## 全局目录详解（~/.echo-agent/）

### 配置文件

| 文件 | 说明 | 敏感 |
|------|------|------|
| `config.yaml` | 主配置文件 | 可能含 Token |
| `credentials.yaml` | 加密凭据存储 | 是 |

### 主数据库

只有一个 SQLite 数据库，不是按子系统分文件。路径由 `storage.database_path` 指定，默认 `data/echo_agent.db`：

| 文件 | 说明 | 大小范围 |
|------|------|----------|
| `echo_agent.db` | 主数据库（会话、任务、调度、成本、进化等） | 10MB ~ 1GB |
| `echo_agent.db-wal` | WAL 日志 | 动态 |
| `echo_agent.db-shm` | 共享内存 | 动态 |

数据库包含的主要表：

| 表名 | 用途 |
|------|------|
| `sessions` | 会话记录 |
| `messages` | 消息历史 |
| `tool_calls` | 工具调用记录 |
| `cron_jobs` | 定时任务 |
| `cost_records` | 费用记录 |
| `approvals` | 审批记录 |
| `migrations` | 迁移状态 |

### data/memory/

| 路径 | 说明 |
|------|------|
| `long_term.json` | 结构化长期记忆（键值对） |
| `embeddings/` | 记忆向量索引（用于语义搜索） |

### data/knowledge/

| 路径 | 说明 |
|------|------|
| `documents/` | 原始知识文档（PDF/MD/TXT 等） |
| `chunks/` | 文档分块存储（JSON 格式） |
| `index/` | 向量检索索引文件 |

### data/spill/

大内容溢出存储。当工具输出或上下文超过阈值时，内容写入此目录并返回引用 ID。

产物按会话分目录存放，文件名形如 `<随机十六进制>-<工具名>.txt`，是纯文本而非 JSON。

!!! tip "自动清理"
    默认保留 7 天（`spill.retention_days`），目录总量上限 512 MB（`spill.max_total_mb`），清理每 6 小时扫描一次（`spill.sweep_interval_hours`）。超量时会在保留期之外继续删除最旧的产物。

### 用户产物目录

`data/artifacts/` 保存由 `artifact_*` 工具创建并交付给用户的报告。它与模型私有的 spill 目录完全分离，布局为会话哈希、随机产物 ID、分块日志及清单；模型不会看到内部路径。

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `artifacts.root_dir` | `data/artifacts` | 工作区内的专用相对目录 |
| `artifacts.max_chunk_chars` | `3000` | 单次追加字符上限；避免工具参数本身撞上模型输出上限 |
| `artifacts.max_artifact_mb` | `50` | 单产物体积上限 |
| `artifacts.retention_days` | `30` | 产物保留天数 |
| `artifacts.max_total_mb` | `1024` | 全部产物总体积上限 |
| `artifacts.sweep_interval_hours` | `24` | 清理周期 |

清理器只删除具有合法会话哈希、产物 ID 和匹配 manifest 的目录，不会遍历或删除不认识的目录形状。总配额清理会保护最近一小时内仍在更新的草稿，避免与正在生成的报告竞争。

### data/logs/

| 文件 | 说明 |
|------|------|
| `tool_audit.jsonl` | 工具调用审计 |
| `memory_audit.jsonl` | 记忆读写审计 |
| `loop_freeze.log` | 事件循环卡死时的看门狗转储 |

日志本身不做按大小或按日期的轮转（配置中没有 `observability.log_rotation`）。受限的是追踪文件数量，由 `observability.max_trace_files` 按条数裁剪，默认 500，设为 0 或负数则不裁剪。

日志使用 loguru 格式：

```
2024-01-15 12:00:00.123 | INFO     | echo_agent.core:run:42 - Agent started
2024-01-15 12:00:01.456 | DEBUG    | echo_agent.tools.search:execute:78 - Search query: "天气"
```

### data/checkpoints/

每个检查点是一个独立目录：

```
chk_20240115_120000/
├── manifest.json          # 检查点元数据
├── state.json             # Agent 状态快照
├── memory_snapshot.json   # 记忆快照
└── db_snapshot.sql        # 数据库快照（增量）
```

### skills/

| 目录 | 说明 |
|------|------|
| `installed/` | 正式安装并激活的技能 |
| `staged/` | 待人工审批的技能（进化产生或外部安装） |
| `evolved/` | 进化系统产生的候选技能 |

每个技能目录结构：

```
<skill_name>/
├── manifest.yaml          # 技能描述与元数据
├── handler.py             # 技能处理逻辑
├── tests/                 # 技能测试
└── eval_results.json      # 评估结果
```

### plugins/

每个插件一个独立目录，结构由插件自行定义。

### cache/

缓存文件为临时数据，可安全删除。

| 目录 | 说明 | 默认过期 |
|------|------|----------|
| `models/` | LLM 响应缓存 | 1 小时 |
| `web/` | 网页内容缓存 | 4 小时 |

---

## 工作区目录详解（.echo-agent/）

工作区目录位于项目根目录下，包含项目级配置和数据。

### config.yaml

工作区配置，覆盖全局配置中的对应字段：

```yaml
# .echo-agent/config.yaml
tools:
  profile: coding
  restrict_to_workspace: true
  exec:
    security: allowlist
    allowed_commands:
      - pytest
      - ruff
```

### memory/

项目级记忆，仅在此工作区内生效。

### knowledge/

项目本地知识库，通常包含项目文档、README、API 文档等。

### sessions/

工作区内的会话历史。每个会话一个 JSON 文件。

---

## 权限与安全

### 推荐文件权限（Linux/macOS）

| 路径 | 权限 | 说明 |
|------|------|------|
| `~/.echo-agent/` | `700` | 仅所有者可访问 |
| `config.yaml` | `600` | 可能含敏感配置 |
| `credentials.yaml` | `600` | 加密凭据 |
| `data/echo_agent.db` | `600` | 数据库文件 |
| `data/logs/` | `700` | 日志目录 |
| `skills/` | `700` | 技能代码目录 |

### .gitignore 建议

对于工作区目录，推荐在 `.gitignore` 中添加：

```gitignore
# Echo Agent 工作区数据
.echo-agent/memory/
.echo-agent/sessions/
.echo-agent/knowledge/local_docs/
```

!!! warning "不要忽略配置"
    `.echo-agent/config.yaml` 通常需要纳入版本控制（团队共享配置），但确保其中不含明文凭据。

---

## 磁盘空间估算

| 组件 | 典型大小 | 增长速度 |
|------|----------|----------|
| SQLite 数据库 | 10MB ~ 1GB | 约 1MB/天（中等使用） |
| 记忆存储 | 1MB ~ 100MB | 缓慢 |
| 知识库 | 取决于文档量 | 手动添加 |
| 审计日志 | 持续增长 | 需自行用系统 logrotate 管理 |
| 检查点 | 50MB ~ 5GB | 每小时一个 |
| 溢出存储 | 0 ~ 500MB | 自动清理 |
| 缓存 | 0 ~ 200MB | 自动过期 |

!!! tip "磁盘清理"
    `echo-agent checkpoint prune` 按配置的保留策略回收检查点，该命令不接受时间参数。保留上限由 `checkpoint.maxSnapshotsPerWorkspace`（默认每工作区 20 个快照）与 `checkpoint.maxTotalSizeMb`（默认 500 MB，超出触发 gc）共同决定。

    工具输出产物在 `data/spill/`，由 `spill.retentionDays` 与 `spill.maxTotalMb` 自动回收，无需手工清理。

### 跨平台路径

全局目录一律取 `Path.home() / ".echo-agent"`，不按平台分支。因此：

| 平台 | 实际位置 |
|------|----------|
| Linux / macOS | `~/.echo-agent/` |
| Windows | `%USERPROFILE%\.echo-agent\` |

Windows 上**不使用** `%APPDATA%`。文档中的 `~/.echo-agent/` 写法在三个平台都指向上表中的位置。
