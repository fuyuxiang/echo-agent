# Gateway 反向代理配置

在生产环境中，通常需要将 Gateway 部署在反向代理（如 nginx 或 Caddy）之后，以实现 HTTPS 终结、域名路由、负载均衡等功能。

## 为什么需要反向代理

- **HTTPS/TLS 终结**：在代理层处理 SSL 证书，Gateway 本身仅需处理 HTTP
- **域名路由**：通过同一个 443 端口对外提供多个服务
- **负载均衡**：多实例部署时分发请求
- **安全加固**：隐藏内部端口，统一入口管控
- **静态资源**：代理层直接服务仪表盘前端文件

## Nginx 配置

### 基础 HTTP + WebSocket 代理

```nginx
upstream echo_gateway {
    server 127.0.0.1:8090;
}

server {
    listen 443 ssl http2;
    server_name gateway.example.com;

    ssl_certificate     /etc/ssl/certs/gateway.example.com.pem;
    ssl_certificate_key /etc/ssl/private/gateway.example.com.key;

    # HTTP API 代理
    location / {
        proxy_pass http://echo_gateway;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # WebSocket 代理 — 会话
    location /ws/session {
        proxy_pass http://echo_gateway;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket 超时设置
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    # WebSocket 代理 — 仪表盘
    location /ws/dashboard {
        proxy_pass http://echo_gateway;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    # 健康检查（可选：限制仅内网访问）
    location /health {
        proxy_pass http://echo_gateway;
        # allow 10.0.0.0/8;
        # deny all;
    }
}
```

!!! tip "WebSocket 超时"
    默认 nginx 的 `proxy_read_timeout` 为 60 秒，WebSocket 长连接会因此被断开。建议设置为 3600 秒或更长。Gateway 自身会通过 ping/pong 帧维持连接活性。

### HTTP 重定向到 HTTPS

```nginx
server {
    listen 80;
    server_name gateway.example.com;
    return 301 https://$host$request_uri;
}
```

## Caddy 配置

Caddy 自动管理 HTTPS 证书，配置相对简洁：

```caddyfile
gateway.example.com {
    reverse_proxy 127.0.0.1:8090

    # Caddy 自动处理 WebSocket upgrade，无需额外配置
    # 但可以显式设置超时
    reverse_proxy /ws/* 127.0.0.1:8090 {
        transport http {
            keepalive 3600s
        }
    }
}
```

!!! tip "Caddy 的优势"
    Caddy 自动从 Let's Encrypt 获取并续期 TLS 证书，自动处理 WebSocket 协议升级，无需额外配置 Upgrade/Connection 头。对于简单部署场景，Caddy 是更省心的选择。

## 必须转发的请求头

无论使用哪种反向代理，以下请求头必须正确转发：

| 请求头 | 用途 | 示例值 |
|--------|------|--------|
| `Host` | 原始主机名，用于 allowed_hosts 校验 | `gateway.example.com` |
| `X-Forwarded-For` | 客户端真实 IP，用于速率限制和审计 | `203.0.113.50` |
| `X-Forwarded-Proto` | 原始协议，用于生成正确的重定向 URL | `https` |
| `Upgrade` | WebSocket 协议升级 | `websocket` |
| `Connection` | 配合 Upgrade 使用 | `upgrade` |

!!! warning "X-Forwarded-For 信任链"
    Gateway 需要正确识别客户端真实 IP 以执行速率限制和审计日志。确保代理链中每一层都正确追加 `X-Forwarded-For`，且 Gateway 配置信任的代理 IP 范围。

## WebSocket 注意事项

### 超时与保活

- 代理层 `read_timeout` 应大于 Gateway 的 ping 间隔（默认 30 秒）
- 建议设置为 3600 秒以支持长时间空闲连接
- Gateway 会定期发送 WebSocket ping 帧维持连接

### 缓冲设置

```nginx
# 禁用代理缓冲以降低流式输出延迟
proxy_buffering off;
```

### 连接数限制

```nginx
# 限制单 IP WebSocket 并发连接数
limit_conn_zone $binary_remote_addr zone=ws_conn:10m;

location /ws/ {
    limit_conn ws_conn 10;
    # ... 其他配置
}
```

## Gateway 端配置

### allowed_hosts

当 Gateway 位于反向代理之后时，需要配置 `allowed_hosts` 以匹配代理转发过来的 Host 头：

```yaml
gateway:
  auth:
    allowed_hosts:
      - "gateway.example.com"
      - "gateway.internal.example.com"  # 内网域名（如有）
```

!!! warning "环回地址检测失效"
    当 Gateway 位于反向代理之后时，所有请求的来源 IP 都会变为 `127.0.0.1`（代理的地址）。这意味着环回地址豁免对所有请求都会生效。务必：
    
    1. 配置 `allowed_hosts` 严格限制允许的 Host 头
    2. 对敏感接口使用令牌认证（token auth）而非依赖环回豁免
    3. 确保代理层本身有适当的访问控制

### allowed_origins

使用自定义域名时，更新 CORS 配置以匹配实际访问域名：

```yaml
gateway:
  auth:
    allowed_origins:
      - "https://gateway.example.com"
      - "https://dashboard.example.com"
```

## 健康检查端点

`/health` 端点可用于负载均衡器的健康探测：

```nginx
# nginx upstream 健康检查（需 nginx plus 或第三方模块）
upstream echo_gateway {
    server 127.0.0.1:8090;
    # health_check uri=/health interval=10s;
}
```

对于 Kubernetes 或云负载均衡器：

```yaml
# Kubernetes Ingress 健康检查注解示例
annotations:
  nginx.ingress.kubernetes.io/health-check-path: /health
  nginx.ingress.kubernetes.io/health-check-interval: "10"
```

!!! tip "健康检查频率"
    建议健康检查间隔设置为 10-30 秒。过于频繁的健康检查会增加日志噪音，但不会对 Gateway 性能产生实质影响。

## 完整部署示例

以下是一个典型的生产部署配置组合：

**Gateway 配置（config.yaml）：**

```yaml
gateway:
  enabled: true
  host: "127.0.0.1"  # 仅监听本地，由代理转发
  port: 8090
  auth:
    mode: "allowlist"
    allowed_users:
      - "telegram:123456"
      - "wechat:wx_admin"
    api_tokens:
      - "tk-external-service-001"
    admin_tokens:
      - "atk-ops-team-master"
    allowed_hosts:
      - "gateway.example.com"
    allowed_origins:
      - "https://dashboard.example.com"
```

**Nginx 配置要点：**

```nginx
server {
    listen 443 ssl http2;
    server_name gateway.example.com;
    
    # TLS 配置省略...
    
    location / {
        proxy_pass http://127.0.0.1:8090;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
    
    location /ws/ {
        proxy_pass http://127.0.0.1:8090;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_read_timeout 3600s;
    }
}
```

!!! question "需维护者确认"
    Gateway 是否支持通过配置声明受信任的代理 IP 范围（trusted_proxies），以便正确解析 `X-Forwarded-For` 链中的客户端真实 IP？

## 相关文档

- [Gateway 概览](index.md)
- [认证详解](authentication.md)
