# 文件系统布局

Echo Agent 的数据存储分为全局目录和工作区目录两个层次。

## 目录结构总览

```
~/.echo-agent/                    # 全局数据目录
├── config.yaml                   # 全局配置文件
├── credentials.yaml              # 凭据存储（加密）
├── data/
│   ├── sqlite/
│   │   └── echo.db              # 主 SQLite 数据库
│   ├── memory/
│   │   ├── long_term.json       # 长期记忆
│   │   └── embeddings/          # 记忆向量索引
│   ├── knowledge/
│   │   ├── documents/           # 知识库原始文档
│   │   ├── chunks/              # 分块后的文档片段
│   │   └── index/               # 向量检索索引
│   ├── spill/                   # 大文件溢出存储
│   ├── logs/
│   │   ├── echo-agent.log       # 主日志文件
│   │   └── archive/             # 归档日志
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

### data/sqlite/

| 文件 | 说明 | 大小范围 |
|------|------|----------|
| `echo.db` | 主数据库（会话、任务、调度等） | 10MB ~ 1GB |
| `echo.db-wal` | WAL 日志 | 动态 |
| `echo.db-shm` | 共享内存 | 动态 |

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

| 文件命名 | 格式 | 说明 |
|----------|------|------|
| `<spill_id>.json` | JSON | 溢出内容 + 元数据 |

!!! tip "自动清理"
    溢出文件默认 24 小时后自动清理。可通过 `spill.retention_hours` 配置。

### data/logs/

| 文件 | 说明 | 轮转策略 |
|------|------|----------|
| `echo-agent.log` | 当前日志 | 按大小轮转（10MB） |
| `archive/echo-agent.log.<date>` | 归档日志 | 保留 7 天 |

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
agent:
  persona: "你是这个项目的开发助手"
tools:
  profile: coding
  overrides:
    filesystem:
      allowed_paths:
        - ./src
        - ./docs
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
| `data/sqlite/echo.db` | `600` | 数据库文件 |
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
| 日志 | 最大 70MB | 自动轮转 |
| 检查点 | 50MB ~ 5GB | 每小时一个 |
| 溢出存储 | 0 ~ 500MB | 自动清理 |
| 缓存 | 0 ~ 200MB | 自动过期 |

!!! tip "磁盘清理"
    使用 `echo-agent checkpoint prune --older-than 7d` 清理旧检查点。缓存可直接删除 `~/.echo-agent/cache/` 目录。

!!! question "需维护者确认"
    Windows 平台下全局数据目录的位置是 `%APPDATA%\echo-agent\` 还是 `%USERPROFILE%\.echo-agent\`？当前文档以 `~/.echo-agent/` 描述。
