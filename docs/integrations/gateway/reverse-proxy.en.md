# Gateway Reverse Proxy Setup

In production environments, Gateway is typically deployed behind a reverse proxy (such as nginx or Caddy) for HTTPS termination, domain routing, load balancing, and other capabilities.

## Why Use a Reverse Proxy

- **HTTPS/TLS termination**: Handle SSL certificates at the proxy layer; Gateway only needs to handle plain HTTP
- **Domain routing**: Serve multiple services through a single port 443
- **Load balancing**: Distribute requests across multiple instances
- **Security hardening**: Hide internal ports, centralize access control
- **Static assets**: Serve dashboard frontend files directly from the proxy layer

## Nginx Configuration

### Basic HTTP + WebSocket Proxy

```nginx
upstream echo_gateway {
    server 127.0.0.1:8090;
}

server {
    listen 443 ssl http2;
    server_name gateway.example.com;

    ssl_certificate     /etc/ssl/certs/gateway.example.com.pem;
    ssl_certificate_key /etc/ssl/private/gateway.example.com.key;

    # HTTP API proxy
    location / {
        proxy_pass http://echo_gateway;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # WebSocket proxy — session
    location /ws/session {
        proxy_pass http://echo_gateway;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket timeout settings
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    # WebSocket proxy — dashboard
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

    # Health check (optional: restrict to internal network)
    location /health {
        proxy_pass http://echo_gateway;
        # allow 10.0.0.0/8;
        # deny all;
    }
}
```

!!! tip "WebSocket Timeout"
    The default nginx `proxy_read_timeout` is 60 seconds, which will cause WebSocket long-lived connections to be terminated. Set it to 3600 seconds or longer. Gateway maintains connection liveness through ping/pong frames.

### HTTP to HTTPS Redirect

```nginx
server {
    listen 80;
    server_name gateway.example.com;
    return 301 https://$host$request_uri;
}
```

## Caddy Configuration

Caddy automatically manages HTTPS certificates, making the configuration much simpler:

```caddyfile
gateway.example.com {
    reverse_proxy 127.0.0.1:8090

    # Caddy handles WebSocket upgrade automatically — no extra config needed
    # But you can explicitly set timeouts
    reverse_proxy /ws/* 127.0.0.1:8090 {
        transport http {
            keepalive 3600s
        }
    }
}
```

!!! tip "Caddy Advantages"
    Caddy automatically obtains and renews TLS certificates from Let's Encrypt, and handles WebSocket protocol upgrades without additional Upgrade/Connection header configuration. For simple deployments, Caddy is the lower-maintenance choice.

## Required Forwarded Headers

Regardless of which reverse proxy you use, the following headers must be forwarded correctly:

| Header | Purpose | Example Value |
|--------|---------|---------------|
| `Host` | Original hostname for allowed_hosts validation | `gateway.example.com` |
| `X-Forwarded-For` | Client real IP for rate limiting and audit | `203.0.113.50` |
| `X-Forwarded-Proto` | Original protocol for generating correct redirect URLs | `https` |
| `Upgrade` | WebSocket protocol upgrade | `websocket` |
| `Connection` | Used with Upgrade | `upgrade` |

!!! warning "X-Forwarded-For Trust Chain"
    Gateway needs to correctly identify the client's real IP for rate limiting and audit logging. Ensure each layer in the proxy chain correctly appends to `X-Forwarded-For`, and configure Gateway with trusted proxy IP ranges.

## WebSocket Considerations

### Timeouts and Keepalive

- Proxy layer `read_timeout` should exceed Gateway's ping interval (default 30 seconds)
- Recommend setting to 3600 seconds to support long-idle connections
- Gateway periodically sends WebSocket ping frames to maintain connection liveness

### Buffering

```nginx
# Disable proxy buffering to reduce streaming output latency
proxy_buffering off;
```

### Connection Limits

```nginx
# Limit WebSocket concurrent connections per IP
limit_conn_zone $binary_remote_addr zone=ws_conn:10m;

location /ws/ {
    limit_conn ws_conn 10;
    # ... other config
}
```

## Gateway-Side Configuration

### allowed_hosts

When Gateway is behind a reverse proxy, configure `allowed_hosts` to match the Host header forwarded by the proxy:

```yaml
gateway:
  auth:
    allowed_hosts:
      - "gateway.example.com"
      - "gateway.internal.example.com"  # internal domain (if applicable)
```

!!! warning "Loopback Detection Bypass"
    When Gateway is behind a reverse proxy, all requests will appear to originate from `127.0.0.1` (the proxy's address). This means the loopback exemption will apply to all requests. You must:
    
    1. Configure `allowed_hosts` to strictly limit allowed Host headers
    2. Use token authentication for sensitive endpoints rather than relying on loopback exemption
    3. Ensure the proxy layer itself has appropriate access controls

### allowed_origins

When using a custom domain, update the CORS configuration to match the actual access domain:

```yaml
gateway:
  auth:
    allowed_origins:
      - "https://gateway.example.com"
      - "https://dashboard.example.com"
```

## Health Check Endpoint

The `/health` endpoint can be used for load balancer health probes:

```nginx
# nginx upstream health check (requires nginx plus or third-party module)
upstream echo_gateway {
    server 127.0.0.1:8090;
    # health_check uri=/health interval=10s;
}
```

For Kubernetes or cloud load balancers:

```yaml
# Kubernetes Ingress health check annotation example
annotations:
  nginx.ingress.kubernetes.io/health-check-path: /health
  nginx.ingress.kubernetes.io/health-check-interval: "10"
```

!!! tip "Health Check Frequency"
    A health check interval of 10-30 seconds is recommended. Overly frequent checks add log noise but have no material impact on Gateway performance.

## Complete Deployment Example

Here is a typical production deployment configuration combination:

**Gateway configuration (config.yaml):**

```yaml
gateway:
  enabled: true
  host: "127.0.0.1"  # Listen only on localhost, forwarded by proxy
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

**Nginx configuration highlights:**

```nginx
server {
    listen 443 ssl http2;
    server_name gateway.example.com;
    
    # TLS config omitted...
    
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

## On X-Forwarded-For

The gateway has no `trusted_proxies` setting and does not derive the client IP from `X-Forwarded-For`, `X-Real-IP` or any other forwarded header. Every trust decision reads the real TCP peer address (the socket's `peername`), never `request.remote` and never a request header.

This is deliberate: forwarded headers are client-controlled, so basing a "this came from localhost" verdict on one would let any remote caller forge local trust by setting a header. The trade-off is that behind a reverse proxy the peer address the gateway sees is the proxy's own.

Two consequences worth planning for:

- **The loopback exemption applies to the proxy, not the end user.** With proxy and gateway on the same host, the gateway sees a loopback peer, so every request arriving through the proxy inherits local trust. Such a deployment must authenticate at the proxy layer; the gateway's loopback check cannot distinguish users.
- **Audit logs record the proxy's address.** If you need end-user IPs, log them at the reverse proxy — the gateway's logs cannot supply them.

Also configure `gateway.auth.allowed_hosts` with the proxy's public domain: bound to a non-loopback address, an empty list rejects every request.

## Related Documentation

- [Gateway Overview](index.en.md)
- [Authentication Details](authentication.en.md)
