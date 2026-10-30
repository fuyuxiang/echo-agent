# Webhook 通道

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../../desktop/index.md)。

Webhook 通道在配置的 HTTP 路径接收入站消息。`channels.webhook` 默认关闭；启用后默认监听 `0.0.0.0:8080` 的 `/webhook`。它不是 Gateway 的 `/api/v1/message` 接口。

## 配置

```yaml
channels:
  webhook:
    enabled: true
    host: 127.0.0.1
    port: 8080
    path: /webhook
    secret: ""
    max_pending: 1000
```

`secret` 为空时不校验签名。对外暴露时应设置非空密钥并配置可信网络入口。通用配置加载器不会展开 YAML 中的环境变量占位符；可由进程环境覆盖：

```bash
export ECHO_AGENT_CHANNELS__WEBHOOK__SECRET="$WEBHOOK_SECRET"
```

## 请求与签名

```bash
curl -X POST http://127.0.0.1:8080/webhook \
  -H 'Content-Type: application/json' \
  -d '{"sender_id":"ci-1","chat_id":"builds","text":"分析构建失败","wait":false}'
```

`text` 必须非空；`sender_id` 和 `chat_id` 未提供时各默认为 `webhook`。`content` 可作为 `text` 的备选字段；`metadata` 为可选对象。`wait` 默认 `false`。

配置非空 `secret` 时，`X-Signature` 的值必须是对**原始请求体字节**计算的 HMAC-SHA256 **十六进制摘要**，不带 `sha256=` 前缀。例如：

```python
import hashlib
import hmac

signature = hmac.new(secret.encode(), request_body_bytes, hashlib.sha256).hexdigest()
headers = {"X-Signature": signature}
```

签名错误或缺失返回 HTTP 403；JSON 或字段错误返回 400。

## 响应

`wait: false` 时，消息成功入队返回 HTTP 200 和 `{"status":"accepted","event_id":"..."}`。`wait: true` 时，连接等待最终回复，成功返回 HTTP 200 和 `{"response":"...","event_id":"..."}`；等待超过 120 秒返回 504。处理失败时响应包含 `status`、`error`、`response`、`event_id`，HTTP 状态由回合结果决定。此通道不提供流式响应或异步回调；如需查看已接受消息的回合状态，可查询 Gateway 的 `GET /api/v1/turns/{event_id}`（需管理权限）。

`max_pending` 只限制同步等待回复的请求数；达到上限时返回 503。入站队列拒绝消息时也返回 503。可用 `Idempotency-Key` 或 `X-Idempotency-Key` 请求头（也可用请求体 `idempotency_key`）安全重试；相同键配不同内容返回 409。详见[Gateway API 的幂等重试](../../reference/gateway-api.md#idempotent-retries)。
