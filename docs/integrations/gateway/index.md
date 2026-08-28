# Gateway 概览

Echo Agent Gateway 是基于 aiohttp 构建的 HTTP/WebSocket 网关服务器，负责将外部消息接入系统内部、管理会话生命周期、执行认证与速率限制，并将消息路由到各平台通道。

## 核心功能

| 功能 | 说明 |
|------|------|
| 外部消息接入 | 通过 HTTP POST 和 WebSocket 接收来自第三方系统的消息 |
| 会话生命周期管理 | 创建、恢复、重置、销毁会话 |
| 认证与速率限制 | 多模式认证 + 令牌桶限流 |
| 跨平台投递路由 | 根据目标平台自动选择投递通道 |
| 渐进式消息编辑 | 支持流式输出时的消息实时更新 |
| 健康监控 | 提供 `/health` 端点用于存活探针 |

## 架构组成

`GatewayServer` 作为主协调器，编排以下子系统：

```
GatewayServer
├── Auth              # 认证模块（多模式）
├── RateLimiter       # 令牌桶速率限制器
├── DeliveryRouter    # 跨平台投递路由
├── ProgressiveEditor # 渐进式消息编辑
├── MediaCache        # 媒体文件缓存
├── SessionResetPolicy # 会话重置策略
└── HookRegistry      # 钩子注册表
```

## 子系统文件

| 文件 | 职责 |
|------|------|
| `auth.py` | 认证逻辑（open / allowlist / pairing 三种模式） |
| `router.py` | 消息投递路由 |
| `rate_limiter.py` | 基于令牌桶的速率限制 |
| `server.py` | aiohttp 应用初始化与路由注册 |
| `health.py` | 健康检查端点 |
| `ws_session.py` | 会话 WebSocket 的平台与会话键归一化 |
| `ws_dashboard.py` | 仪表盘 WebSocket（供监控面板连接） |

## API 模块

Gateway 在 `gateway/api/` 目录下提供以下 REST API 模块：

- `analytics` — 数据统计与分析
- `channels` — 通道管理
- `config` — 运行时配置只读查询
- `cron_api` — 定时任务管理
- `knowledge` — 知识库操作
- `logs` — 日志查询
- `memory` — 记忆存储
- `sessions` — 会话 CRUD
- `skills` — 技能管理
- `tasks` — 异步任务队列

## WebSocket 端点

| 端点 | 用途 | 协议 |
|------|------|------|
| `/ws`（可配置） | CLI 与外部集成实时通信 | JSON over WebSocket |
| `/ws/dashboard` | 监控仪表盘实时数据推送 | JSON over WebSocket |

## 健康检查

```
GET /health
```

返回 `200 OK` 表示服务正常运行，适配 Kubernetes liveness/readiness probe 和负载均衡器健康探测。

## 速率限制

Gateway 使用令牌桶算法进行速率限制，按 `platform + chat_id` 粒度隔离：

- 默认限制：**30 RPM**（每分钟 30 次请求）
- 可通过配置文件调整
- 超限时返回 `429 Too Many Requests`

!!! tip "速率限制粒度"
    限流以 `platform:chat_id` 为 key，不同平台的同一用户分别计数。这意味着同一个用户在 Telegram 和 Web 上各自拥有独立的速率配额。

## 配置示例

```yaml
gateway:
  enabled: true
  host: "0.0.0.0"
  port: 8090
  auth:
    mode: "allowlist"  # open | allowlist | pairing
    allowed_users: ["user1", "telegram:123456"]
    api_tokens: ["token-xxx"]
    admin_tokens: ["admin-xxx"]
    allowed_origins: ["https://my-dashboard.example.com"]
```

!!! warning "生产环境注意"
    在生产环境中务必将 `auth.mode` 设置为 `allowlist` 或 `pairing`，切勿使用 `open` 模式。同时请为 `api_tokens` 和 `admin_tokens` 使用足够长度的随机字符串。

## 默认监听地址

Gateway 默认监听 `127.0.0.1:58123`。端口用 `gateway.port` 配置，主机用 `gateway.host`；两者也可通过 `ECHO_AGENT_GATEWAY_PORT`、`ECHO_AGENT_GATEWAY_HOST` 环境变量覆盖（环境变量前缀为 `ECHO_AGENT_`，路径中的层级用下划线连接）。

`gateway.port` 设为 `0` 时由系统动态分配，实际端口写入 `workspace/.echo-agent/gateway.json`。

## 快速启动

Gateway 是**独立进程**，不随其他命令自动启动：

```bash
echo-agent gateway              # 前台启动
echo-agent gateway install      # 注册为后台常驻服务
```

`echo-agent run` 是自带 agent 的交互式会话，不会顺带起网关；反过来，网关运行后用 `echo-agent cli` 以瘦客户端接入。两者共享同一份状态，但生命周期彼此独立。

启动后访问 `http://127.0.0.1:58123/health` 验证服务状态。

## 相关文档

- [认证详解](authentication.md)
- [反向代理配置](reverse-proxy.md)
