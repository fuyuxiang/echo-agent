# Gateway 反向代理

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../../desktop/index.md)。

以下示例让 Gateway 监听本机 `127.0.0.1:8090`，由反向代理提供 HTTPS。`8090` 是示例端口；默认端口为 `58123`。健康检查默认路径为 `/api/v1/health`；修改 `gateway.api_prefix` 后，应同步修改探针路径。

## Gateway 配置

```yaml
gateway:
  enabled: true
  host: "127.0.0.1"
  port: 8090
  auth:
    mode: "allowlist"
    api_tokens: ["replace-with-a-random-token"]
    allowed_hosts: ["gateway.example.com"]
    allowed_origins: ["https://gateway.example.com"]
```

`allowed_hosts` 匹配代理转发的 `Host` 头，应包含浏览器访问使用的域名。`allowed_origins` 是允许的浏览器 Origin 列表，不会关闭跨站请求检查。`0.0.0.0` 和 `::` 不是有效的 Host 条目。还应按实际用户配置 `allowed_users`，或使用配对模式；API 令牌与用户授权是两项独立检查。

`echo-agent run` 在 `gateway.enabled: true` 时启动 Gateway；也可以单独执行 `echo-agent gateway`。确认代理目标端口与 `gateway.port` 一致。

## nginx

将 `map` 和 `server` 块放在 nginx 的 `http` 上下文中。单个 `location` 同时代理 HTTP、`/ws` 和 `/ws/dashboard`。

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 443 ssl;
    server_name gateway.example.com;
    ssl_certificate     /etc/ssl/certs/gateway.example.com.pem;
    ssl_certificate_key /etc/ssl/private/gateway.example.com.key;

    location / {
        proxy_pass http://127.0.0.1:8090;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
        proxy_buffering off;
    }
}
```

证书路径需替换为实际文件。`proxy_read_timeout` 应覆盖预计的空闲连接时间。Gateway 默认每 30 秒发送一次 WebSocket 心跳，可用 `gateway.ws_heartbeat_seconds` 调整。

## Caddy

```caddyfile
gateway.example.com {
    reverse_proxy 127.0.0.1:8090
}
```

Caddy 的 `reverse_proxy` 可代理 HTTP 和 WebSocket。证书获取方式取决于域名与 Caddy 的部署配置。

## 客户端地址与认证

Gateway 没有 `trusted_proxies` 配置项，也不会根据 `X-Forwarded-For` 或 `X-Real-IP` 判定客户端地址或本机信任。相关判断使用 TCP 连接的真实对端地址。Gateway 的限流按 `platform:chat_id` 分桶，不按客户端 IP 分桶。

代理与 Gateway 同机且通过 `127.0.0.1` 连接时，Gateway 看到的是代理的回环地址。本机来源可通过消息接入时的用户白名单检查，但 API 令牌、平台身份校验和限流仍然生效。不要把回环来源视为最终用户身份；如需按最终用户或 IP 限制访问，应在代理层实施，并由代理记录客户端 IP。

代理需保留客户端认证头。Gateway 默认读取 `X-Echo-Agent-Token`，也接受 `Authorization: Bearer <token>`。浏览器管理操作还检查 Host 和 Origin；`allowed_hosts` 必须包含代理转发的公开域名。

健康探测可访问 `https://gateway.example.com/api/v1/health`。健康状态为 `unhealthy` 时响应 `503`，否则响应 `200`；探针频率和失败阈值由部署平台设置。

## 相关文档

- [Gateway 概览](index.md)
- [认证详解](authentication.md)
