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
| `ws_session.py` | Session WebSocket (for chat clients) |
| `ws_dashboard.py` | Dashboard WebSocket (for monitoring panels) |

## API Modules

Gateway exposes the following REST API modules under `gateway/api/`:

- `analytics` — Statistics and analytics
- `channels` — Channel management
- `chat_attachments` — Chat attachment upload and management
- `config` — Runtime configuration read/write
- `cron_api` — Scheduled task management
- `knowledge` — Knowledge base operations
- `lifecycle` — Service lifecycle control
- `logs` — Log queries
- `memory` — Memory storage
- `sessions` — Session CRUD
- `skills` — Skill management
- `tasks` — Async task queue

## WebSocket Endpoints

| Endpoint | Purpose | Protocol |
|----------|---------|----------|
| `/ws/session` | Real-time communication for chat clients | JSON over WebSocket |
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

## Default Port

Gateway listens on port **8090** by default. Override via the `gateway.port` config key or the `ECHO_GATEWAY_PORT` environment variable.

## Quick Start

1. Enable Gateway in your config file (`gateway.enabled: true`)
2. Configure the authentication mode and allowed users list
3. Start Echo Agent — Gateway will start automatically as a subprocess
4. Visit `http://localhost:8090/health` to verify the service status

!!! question "Maintainer Confirmation Needed"
    Can Gateway be started independently from the main process? This document assumes Gateway starts automatically as a sub-service of the main process.

## Related Documentation

- [Authentication Details](authentication.en.md)
- [Reverse Proxy Setup](reverse-proxy.en.md)
