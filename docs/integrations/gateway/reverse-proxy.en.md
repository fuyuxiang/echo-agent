# Gateway Reverse Proxy

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../../desktop/index.md).

These examples bind the Gateway to `127.0.0.1:8090` and expose HTTPS through a reverse proxy. `8090` is an example port; the default is `58123`. The default health path is `/api/v1/health`; update the probe if `gateway.api_prefix` changes.

## Gateway configuration

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

`allowed_hosts` matches the `Host` header forwarded by the proxy; include the domain used by browsers. `allowed_origins` lists allowed browser Origins and does not disable cross-site request checks. `0.0.0.0` and `::` are not valid Host entries. Configure `allowed_users` for actual users or use pairing mode; API tokens and user authorization are separate checks.

`echo-agent run` starts the Gateway when `gateway.enabled: true`. Alternatively, run `echo-agent gateway` separately. Make sure the proxy target port matches `gateway.port`.

## nginx

Place the `map` and `server` blocks in nginx's `http` context. A single `location` proxies HTTP, `/ws`, and `/ws/dashboard`.

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

Replace the certificate paths with real files. Set `proxy_read_timeout` for the expected idle connection time. The Gateway sends WebSocket heartbeat frames every 30 seconds by default; `gateway.ws_heartbeat_seconds` controls this interval.

## Caddy

```caddyfile
gateway.example.com {
    reverse_proxy 127.0.0.1:8090
}
```

Caddy's `reverse_proxy` handles HTTP and WebSocket traffic. Certificate acquisition depends on the domain and Caddy deployment configuration.

## Client addresses and authentication

The Gateway has no `trusted_proxies` setting. It does not use `X-Forwarded-For` or `X-Real-IP` to identify clients or grant local trust; those decisions use the actual TCP peer address. Rate limiting uses `platform:chat_id` buckets, not client IP buckets.

When the proxy connects over `127.0.0.1`, the Gateway sees the proxy as the loopback peer. Local origin may pass the user allowlist check during message ingestion, but API-token checks, platform identity checks, and rate limiting still apply. Do not treat the loopback peer as the end user's identity. Enforce per-user or per-IP access at the proxy when needed, and record client IPs there.

The proxy must preserve client authentication headers. By default the Gateway reads `X-Echo-Agent-Token` and also accepts `Authorization: Bearer <token>`. Browser administration also checks Host and Origin; `allowed_hosts` must contain the public domain forwarded by the proxy.

Probe `https://gateway.example.com/api/v1/health`. It returns `503` for `unhealthy` status and `200` otherwise; configure probe frequency and failure thresholds in the deployment platform.

## Related documentation

- [Gateway Overview](index.en.md)
- [Authentication Details](authentication.en.md)
