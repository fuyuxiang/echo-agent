# 运行方式

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

`echo-agent run` 和 `echo-agent gateway` 都启动完整 Agent 运行时。区别在于前者按 `gateway.enabled` 决定是否启动 HTTP/WebSocket Gateway；后者在本次运行中启用 Gateway，即使配置文件中未设置 `gateway.enabled: true`。两者默认都在前台运行。`echo-agent cli` 是连接已运行 Gateway 的瘦客户端。

| 命令 | Gateway | 进程管理 | 用途 |
|---|---|---|---|
| `echo-agent run` | 配置 `gateway.enabled: true` 时启用 | 当前终端 | 本地运行与调试 |
| `echo-agent gateway` | 启用 | 当前终端 | 前台网关服务 |
| `echo-agent gateway install`、`start` | 启用 | systemd 或 launchd | 后台服务 |
| `echo-agent cli` | 连接已有 Gateway | 当前终端 | 多终端接入 |

同一工作区通常只应有一个完整 Agent 运行时写入数据；不要同时对同一工作区运行 `run` 与 `gateway`。

## 本地运行

```bash
echo-agent run
```

运行模式由命令决定；配置 `gateway.enabled` 决定 `run` 是否同时开放 Gateway。日志级别由 `observability.log_level` 或 `ECHO_AGENT_OBSERVABILITY__LOG_LEVEL` 设置，`run` 没有 `--log-level` 选项。配置文件搜索规则见[文件系统布局](../reference/filesystem-layout.md)。

## Gateway 服务

```bash
echo-agent gateway                 # 前台
echo-agent gateway install         # 注册后台服务
echo-agent gateway start
echo-agent gateway status
echo-agent gateway logs --follow
```

Gateway 默认监听 `127.0.0.1:58123`，HTTP API 前缀默认为 `/api/v1`。它支持 WebSocket 会话和 Dashboard 连接。管理接口与消息接入具有不同的认证检查；`gateway.auth.mode`（`open`、`allowlist`、`pairing`）控制用户授权，`gateway.auth.api_tokens` 则控制 API 令牌检查。普通 API 令牌不构成完整的多租户资源隔离。配置细节见[Gateway 认证](../integrations/gateway/authentication.md)和[安全模型](../concepts/security-model.md)。

后台服务的安装路径、停止超时和环境变量传递见[后台服务](background-service.md)。

## CLI 客户端

```bash
echo-agent cli
echo-agent cli --tui
echo-agent cli --port 58123 --token your-api-token
```

CLI 客户端通过 WebSocket 连接本机 Gateway，不启动另一套 Agent 逻辑。默认使用终端内联界面，`--tui` 选择全屏界面。连接前应先启动 Gateway；远程部署可通过 SSH 端口转发接入本机回环地址。
