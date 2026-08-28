# Gateway Overview

Echo Agent Gateway is an aiohttp-based HTTP/WebSocket server responsible for ingesting external messages, managing session lifecycles, enforcing authentication and rate limiting, and routing messages to platform-specific delivery channels.

## Core Features

| Feature | Description |
|---------|-------------|
| External message ingestion | Receive messages from third-party systems via HTTP POST and WebSocket |
| Session lifecycle management | Create, resume, reset, and destroy sessions |
| Authentication & rate limiting | Multi-mode auth + token-bucket rate limiting |
| Cross-platform delivery routing | Automatically select delivery channel based on target platform |
| Progressive message editing | Real-time message updates during streaming output |
| Health monitoring | `/health` endpoint for liveness probes |

## Architecture

`GatewayServer` acts as the main orchestrator, coordinating the following subsystems:

```
GatewayServer
├── Auth              # Authentication module (multi-mode)
├── RateLimiter       # Token-bucket rate limiter
├── DeliveryRouter    # Cross-platform delivery routing
├── ProgressiveEditor # Progressive message editing
├── MediaCache        # Media file cache
├── SessionResetPolicy # Session reset policy
└── HookRegistry      # Hook registry
```

## Subsystem Files

| File | Responsibility |
|------|---------------|
| `auth.py` | Authentication logic (open / allowlist / pairing modes) |
| `router.py` | Message delivery routing |
| `rate_limiter.py` | Token-bucket rate limiting |
| `server.py` | aiohttp app initialization and route registration |
| `health.py` | Health check endpoint |
| `ws_session.py` | Platform and session-key normalization for the session WebSocket |
| `ws_dashboard.py` | Dashboard WebSocket (for monitoring panels) |

## API Modules

Gateway exposes the following REST API modules under `gateway/api/`:

- `analytics` — Statistics and analytics
- `channels` — Channel management
- `config` — Read-only runtime configuration query
- `cron_api` — Scheduled task management
- `knowledge` — Knowledge base operations
- `logs` — Log queries
- `memory` — Memory storage
- `sessions` — Session CRUD
- `skills` — Skill management
- `tasks` — Async task queue

## WebSocket Endpoints

| Endpoint | Purpose | Protocol |
|----------|---------|----------|
| `/ws` (configurable) | Real-time communication for CLI and external integrations | JSON over WebSocket |
| `/ws/dashboard` | Real-time data push for monitoring dashboards | JSON over WebSocket |

## Health Check

```
GET /health
```

Returns `200 OK` when the service is running normally. Compatible with Kubernetes liveness/readiness probes and load balancer health checks.

## Rate Limiting

Gateway uses a token-bucket algorithm for rate limiting, isolated by `platform + chat_id` granularity:

- Default limit: **30 RPM** (30 requests per minute)
- Configurable via the configuration file
- Returns `429 Too Many Requests` when exceeded

!!! tip "Rate Limit Granularity"
    Rate limiting uses `platform:chat_id` as the key, counting separately for the same user across different platforms. This means a single user has independent rate quotas on Telegram and Web respectively.

## Configuration Example

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

!!! warning "Production Notice"
    In production, always set `auth.mode` to `allowlist` or `pairing` — never use `open` mode. Use sufficiently long random strings for `api_tokens` and `admin_tokens`.

## Default listen address

The Gateway listens on `127.0.0.1:58123` by default. The port comes from `gateway.port` and the host from `gateway.host`; both can also be overridden with `ECHO_AGENT_GATEWAY_PORT` and `ECHO_AGENT_GATEWAY_HOST` (the prefix is `ECHO_AGENT_`, with underscores joining config path segments).

Setting `gateway.port` to `0` lets the system assign a port; the one actually bound is written to `workspace/.echo-agent/gateway.json`.

## Quick Start

The Gateway is a **standalone process**; no other command starts it implicitly:

```bash
echo-agent gateway              # run in the foreground
echo-agent gateway install      # register as a resident background service
```

`echo-agent run` is an interactive session with its own agent and does not bring a gateway up alongside it. Conversely, once the gateway is running, `echo-agent cli` attaches to it as a thin client. The two share the same state, but their lifecycles are independent.

Once started, visit `http://127.0.0.1:58123/health` to verify the service.

## Related Documentation

- [Authentication Details](authentication.en.md)
- [Reverse Proxy Setup](reverse-proxy.en.md)
