# Echo Agent Dashboard 设计规格

## 概述

为 Echo Agent 构建一个完整的 Web 管理后台，包含 11 个页面：系统概览、会话管理、记忆管理、技能管理、知识库、通道管理、定时任务、看板、日志、配置管理、分析统计。

## 架构

### 项目结构

```
echo-agent/
├── web/                          ← 前端工程（独立 package.json）
│   ├── src/
│   │   ├── App.tsx               ← 根组件：Sidebar + Router
│   │   ├── pages/
│   │   │   ├── Overview/
│   │   │   ├── Sessions/
│   │   │   ├── Memory/
│   │   │   ├── Skills/
│   │   │   ├── Knowledge/
│   │   │   ├── Channels/
│   │   │   ├── Cron/
│   │   │   ├── Kanban/
│   │   │   ├── Logs/
│   │   │   ├── Config/
│   │   │   └── Analytics/
│   │   ├── components/           ← 共享 UI 组件
│   │   ├── lib/                  ← API client, WebSocket, utils
│   │   └── hooks/                ← 自定义 hooks
│   ├── index.html
│   ├── vite.config.ts
│   ├── tailwind.config.ts
│   └── package.json
├── echo_agent/
│   └── gateway/
│       ├── api/
│       │   ├── tasks.py          ← 看板 CRUD API（新增）
│       │   ├── logs.py           ← 日志流 API（新增）
│       │   ├── analytics.py      ← 统计聚合 API（新增）
│       │   └── sessions.py       ← 会话详情 API（扩展）
│       ├── ws_dashboard.py       ← Dashboard WebSocket（新增）
│       └── static/ → web/dist    ← 构建产物
```

### 前后端交互

- REST API：复用现有 `/api/v1/*` 前缀，新增端点
- WebSocket：`/ws/dashboard` 独立端点，推送看板事件、日志流、健康心跳
- 认证：Bearer token（复用现有 admin token）

### 部署模式

- 开发：Vite dev server + proxy 到 aiohttp 后端
- 生产：`vite build` → `dist/` 由 aiohttp 托管，随 wheel 包分发

## 技术栈

### 前端

| 用途 | 方案 |
|------|------|
| 框架 | React 19 + TypeScript |
| 构建 | Vite 6 |
| 样式 | Tailwind CSS 4 + shadcn/ui |
| 路由 | React Router 7 |
| 状态管理 | Zustand |
| 拖拽 | @dnd-kit/core + @dnd-kit/sortable |
| 图表 | Recharts |
| 代码编辑器 | CodeMirror 6 |
| 图标 | Lucide React |
| HTTP | 原生 fetch 封装 |
| WebSocket | 原生 WebSocket + 自动重连 |
| 日期 | date-fns |

### 后端

- 框架：现有 aiohttp gateway，扩展 API
- 数据库：现有 SQLite（aiosqlite）
- WebSocket：aiohttp 原生支持

## 看板（Kanban）设计

### 状态机扩展

新增 `BLOCKED` 和 `REVIEW` 两个状态：

```python
class TaskStatus(str, Enum):
    PENDING = "pending"       # Inbox 列
    QUEUED = "queued"         # Queued 列
    RUNNING = "running"       # Running 列
    BLOCKED = "blocked"       # Blocked 列（新增）
    REVIEW = "review"         # Review 列（新增）
    SUCCESS = "success"       # Done 列
    FAILED = "failed"         # 卡片徽章，不单独成列
    CANCELLED = "cancelled"   # 归档，不显示
    SUSPENDED = "suspended"   # 卡片徽章，不单独成列

VALID_TASK_TRANSITIONS = {
    PENDING:   {QUEUED, CANCELLED},
    QUEUED:    {RUNNING, CANCELLED},
    RUNNING:   {REVIEW, BLOCKED, FAILED, SUSPENDED, CANCELLED},
    BLOCKED:   {QUEUED, RUNNING, CANCELLED},
    REVIEW:    {SUCCESS, QUEUED},
    SUSPENDED: {QUEUED, RUNNING, CANCELLED},
    FAILED:    {QUEUED},
    SUCCESS:   set(),
    CANCELLED: set(),
}
```

### 看板列映射

| 看板列 | TaskStatus | 语义 |
|-------|-----------|------|
| Inbox | PENDING | 新建待分类 |
| Queued | QUEUED | 已确认，排队中 |
| Running | RUNNING | Agent 执行中 |
| Blocked | BLOCKED | 依赖未满足，条件就绪自动恢复 |
| Review | REVIEW | Agent 完成，等人验收 |
| Done | SUCCESS | 完成 |

FAILED/SUSPENDED/CANCELLED 不单独成列，作为卡片状态徽章显示。

### 卡片数据模型扩展

TaskRecord 新增字段：

| 字段 | 类型 | 用途 |
|------|------|------|
| board_id | str | 所属看板（默认 "default"，预留多板） |
| labels | list[str] | 标签，过滤/分组 |
| assignee | str | 指派的 Agent 或人 |
| source | str | 来源（human/agent/cron） |
| session_id | str | 关联执行会话 |
| blocked_reason | str | 阻塞原因 |
| review_summary | str | Agent 执行摘要，供人审阅 |

### 看板交互

- 拖拽卡片 → `POST /api/v1/tasks/{id}/transition`，后端校验状态机
- 非法拖拽 → 前端回弹 + toast 提示
- Agent 创建子任务 → 自动出现在 Inbox 列
- Running 卡片关联会话 → 点击跳转会话详情
- Failed 卡片 → 重试按钮，入 Queued

### 多看板策略

数据模型预留 `board_id` 字段（默认 `"default"`），UI 第一版只实现单看板 + 强过滤（按标签/来源/指派人）。

### 实时推送事件

- `task_created` — 新卡片
- `task_transitioned` — 卡片移列
- `task_updated` — 内容变更
- `task_claimed` — Agent 认领

## 页面功能设计

### Overview（系统概览）

- 顶部：系统状态指示灯（healthy/degraded/unhealthy）+ 运行时长
- 指标卡片行：活跃会话数、通道在线数、记忆条数、今日 token 消耗
- 看板摘要条：各列任务计数（Running 3 / Blocked 1 / Review 2）
- 通道状态列表：每个通道一行，名称 + 类型 + 在线/离线状态
- 最近活动流：最近 10 条事件

### Sessions（会话管理）

- 左侧：会话列表（按时间倒序），搜索框，按通道/用户过滤
- 右侧：选中会话的对话回放（气泡样式），显示工具调用、思考过程
- 操作：删除会话、重置会话状态、导出对话记录

### Memory（记忆管理）

- 四层 Tab：core / episodic / semantic / procedural
- 每层列表展示：内容摘要 + 创建时间 + 衰减权重
- 操作：语义搜索、手动创建/编辑/删除条目

### Skills（技能管理）

- 卡片网格：每个技能一张卡片，名称 + 描述 + 启用开关
- 详情面板：查看技能源码、依赖列表、安装缺失依赖
- 操作：导入新技能（从路径）、删除、启用/禁用

### Knowledge（知识库）

- 文档列表：文件名 + 类型 + 大小 + 索引状态
- 上传区：拖拽上传，支持 PDF/DOCX/TXT/MD
- 索引状态：当前索引健康度、最后重建时间、重建按钮
- 操作：删除文档、手动触发重建索引

### Channels（通道管理）

- 列表视图：通道名 + 类型 + 运行状态
- 展开显示：配置摘要（脱敏）、最近消息数、错误计数
- 只读展示（修改走 Config 页面或 CLI）

### Cron（定时任务）

- 表格：任务名 + cron 表达式 + 下次执行时间 + 最近状态
- 操作：创建、编辑、删除、手动触发、查看执行历史
- 展开显示：最近 5 次执行结果

### Logs（日志）

- 实时日志流（WebSocket 推送）
- 日志级别过滤：DEBUG / INFO / WARNING / ERROR
- 关键词搜索
- 自动滚动 + 暂停按钮
- 时间范围筛选（历史日志）

### Config（配置管理）

- CodeMirror 编辑器展示当前配置（脱敏敏感字段）
- 分区展示：模型配置、通道配置、安全配置、存储配置
- 可编辑的安全子集（日志级别、session 超时），确认后生效

### Analytics（分析统计）

- 时间范围选择器（今天/7天/30天/自定义）
- Token 消耗趋势图（按天、按模型）
- 技能调用频次排行
- 通道消息量分布
- 平均响应时间趋势

## 后端 API 新增

```
# tasks.py — 看板 CRUD
GET    /api/v1/tasks                  # 列表（支持 status/label/assignee 过滤）
POST   /api/v1/tasks                  # 创建
GET    /api/v1/tasks/{id}             # 详情
PUT    /api/v1/tasks/{id}             # 更新字段
POST   /api/v1/tasks/{id}/transition  # 状态流转
DELETE /api/v1/tasks/{id}             # 删除/归档

# sessions.py — 会话详情
GET    /api/v1/sessions               # 扩展过滤参数
GET    /api/v1/sessions/{key}/history # 对话记录

# logs.py — 日志查询
GET    /api/v1/logs                   # 历史日志（分页 + 过滤）

# analytics.py — 统计聚合
GET    /api/v1/analytics/tokens       # Token 消耗（按天/模型）
GET    /api/v1/analytics/skills       # 技能调用频次
GET    /api/v1/analytics/channels     # 通道消息量

# cron.py — 定时任务 CRUD
GET    /api/v1/cron                   # 列表
POST   /api/v1/cron                   # 创建
PUT    /api/v1/cron/{id}              # 编辑
DELETE /api/v1/cron/{id}              # 删除
POST   /api/v1/cron/{id}/trigger      # 手动触发
GET    /api/v1/cron/{id}/runs         # 执行历史
```

## WebSocket 协议（/ws/dashboard）

```json
// 客户端 → 服务端：订阅
{"type": "subscribe", "channels": ["tasks", "logs", "health"]}

// 服务端 → 客户端：事件推送
{"type": "task_transitioned", "payload": {"id": "t_abc", "from": "running", "to": "review"}}
{"type": "log_entry", "payload": {"level": "INFO", "message": "...", "ts": "..."}}
{"type": "health_tick", "payload": {"status": "healthy", "sessions": 3}}
```

## 认证

1. 用户访问 Dashboard → 检查 localStorage 中的 token
2. 无 token → 登录页 → 输入 admin token → 验证后存入 localStorage
3. API 请求 Header: `Authorization: Bearer <token>`
4. WebSocket 通过首条消息发送 token 认证

## 构建与分发

- `web/package.json` 的 `build` 输出到 `web/dist/`
- `pyproject.toml` 的 `force-include` 加入 `"web/dist" = "echo_agent/_bundled/dashboard"`
- aiohttp 启动时检测 `_bundled/dashboard/` 或 `../web/dist/`，存在则托管
- Gateway 的 `GET /` 优先返回 dashboard，原 playground 移至 `/playground`
