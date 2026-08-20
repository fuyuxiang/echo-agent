# Webhook（通用）通道

## 概述

Webhook 通道启动一个 HTTP 服务器，接收外部系统发送的 POST 请求。适用于 CI/CD 集成、自定义应用对接、自动化测试等场景。无需依赖第三方平台 SDK，任何能发送 HTTP 请求的系统都可以接入。

本通道标记为 `is_realtime=False`——默认异步处理，但支持同步等待模式（`wait: true`），请求端可阻塞直到 Agent 回复就绪。

## 配置示例

```yaml
channels:
  webhook:
    enabled: true
    host: 0.0.0.0
    port: 8080
    path: /webhook
    secret: ${WEBHOOK_SECRET}
    max_pending: 1000
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `host` | string | 0.0.0.0 | 监听地址 |
| `port` | int | 8080 | 监听端口 |
| `path` | string | /webhook | 接收请求的路径 |
| `secret` | string | — | HMAC-SHA256 签名验证密钥 |
| `max_pending` | int | 1000 | 待处理请求队列上限 |

## 回调/Webhook 设置

### 请求格式

向配置的端点发送 POST 请求：

```bash
curl -X POST "http://localhost:8080/webhook" \
  -H "Content-Type: application/json" \
  -H "X-Signature: sha256=<hmac_hex_digest>" \
  -d '{
    "sender_id": "ci-pipeline-42",
    "chat_id": "build-notifications",
    "text": "构建失败，请分析日志并给出修复建议",
    "wait": true
  }'
```

| 请求字段 | 类型 | 必填 | 说明 |
|----------|------|------|------|
| `sender_id` | string | 是 | 发送者标识 |
| `chat_id` | string | 是 | 会话标识（用于上下文隔离） |
| `text` | string | 是 | 消息文本内容 |
| `wait` | bool | 否 | 是否同步等待回复（默认 false） |

### 签名验证

请求必须携带 `X-Signature` 头，格式为 `sha256=<hex_digest>`。签名计算方式：

```python
import hmac, hashlib

signature = hmac.new(
    secret.encode(),
    request_body_bytes,
    hashlib.sha256
).hexdigest()

header_value = f"sha256={signature}"
```

!!! warning "签名验证失败"
    缺少 `X-Signature` 头或签名不匹配时，服务器返回 `401 Unauthorized`。确保签名使用原始请求体字节计算（不要重新序列化 JSON）。

### 响应行为

**异步模式** (`wait: false` 或省略)：

```json
HTTP 202 Accepted
{"status": "accepted", "correlation_id": "uuid-xxx"}
```

**同步模式** (`wait: true`)：

```json
HTTP 200 OK
{
  "correlation_id": "uuid-xxx",
  "response": "根据日志分析，构建失败原因是..."
}
```

!!! tip "同步模式超时"
    同步等待受 HTTP 客户端超时限制。对于需要长时间处理的请求，建议使用异步模式配合轮询或回调机制。

## 能力矩阵

| 能力 | 支持 | 说明 |
|------|------|------|
| 编辑已发消息 | ❌ | HTTP 响应后不可修改 |
| 表情回应 | ❌ | — |
| 文件附件 | ❌ | 不支持文件上传 |
| 实时流式输出 | ❌ | 异步投递或同步阻塞 |
| 群组/多人会话 | ❌ | 按 chat_id 独立会话 |

## 内部机制

### 关联式响应投递

每个入站请求分配唯一 `correlation_id`。在同步模式下，HTTP 连接保持打开直到 Agent 产生与该 correlation_id 匹配的响应。在异步模式下，响应通过 correlation_id 关联后投递。

### 队列限流

```yaml
max_pending: 1000
```

当待处理队列达到 `max_pending` 上限时，新请求将收到 `429 Too Many Requests` 响应。这防止了突发流量导致的内存溢出。

## 常见问题

!!! question "如何在外网暴露 Webhook？"
    在开发环境可使用 ngrok 或 cloudflared tunnel 将本地端口映射到公网 URL。生产环境建议通过反向代理（Nginx/Caddy）处理 TLS 终止。

!!! question "secret 为空会怎样？"
    如果未配置 `secret`，签名验证将被跳过。这仅适用于受信任内网环境，生产部署必须配置密钥。

!!! question "max_pending 达到上限后如何恢复？"
    当队列中的请求被处理完成后，自然腾出空间。不需要手动干预。如果持续触发限流，应考虑增加 Agent 处理能力或调大队列上限。

!!! note "响应是一次性的 JSON，不支持流式"
    通道只以 `application/json` 一次性返回完整回复，没有 SSE 或分块传输模式。调用方需要等待整轮推理结束，因此要把客户端超时设得足够长；服务端等待超时会返回 `504`。

    需要增量输出时改用网关的 WebSocket 接口，它按 token 推送流式内容，参见 [WebSocket 协议](../../reference/websocket-protocol.md)。
