# Gateway API Reference

The Echo Agent gateway exposes a RESTful HTTP API for programmatic access to agent capabilities. By default it binds to `127.0.0.1:58123` with the API prefix `/api/v1`.

## Authentication

### Token-Based Auth

All API requests require an authentication token (unless `security.profile` is `minimal`).

```bash
curl -H "Authorization: Bearer ea_tok_abc123..." \
  http://localhost:3007/api/sessions
```

The token header name defaults to `Authorization` with a `Bearer` prefix. Custom header names can be configured:

```yaml
gateway:
  auth:
    token_header: X-Echo-Agent-Token
```

### Auth Modes

| Mode | Behavior |
|------|----------|
| `open` | No authentication required |
| `allowlist` | Only tokens in `api_tokens` list accepted |
| `pairing` | New clients must complete a pairing handshake before accessing the API |

```yaml
gateway:
  auth:
    mode: allowlist
    api_tokens:
      - "ea_tok_production_01"
      - "ea_tok_ci_runner"
    admin_tokens:
      - "ea_adm_superuser"
```

### Admin vs API Tokens

| Capability | API Token | Admin Token |
|-----------|-----------|-------------|
| Send messages; read memory, knowledge status, and analytics | ✓ | ✓ |
| Read configuration; manage sessions and tasks | — | ✓ |
| Manage cron jobs | — | ✓ |
| Manage skills and knowledge documents | — | ✓ |

### Pairing Flow

When `mode: pairing` is active, generate a short-lived code for a platform:

```
POST /api/v1/pair
Content-Type: application/json

{"platform": "telegram"}
```

Response:

```json
{
  "code": "A1B2C3D4E5",
  "ttl_seconds": 300
}
```

The client then verifies that code for its user identity:

```http
POST /api/v1/pair/verify
Content-Type: application/json

{"platform": "telegram", "user_id": "user-123", "code": "A1B2C3D4E5"}
```

A successful response is `{"status":"paired"}`. Pairing authorizes that platform/user pair; it does not issue a new API token.

!!! tip "Pairing TTL"
    Unapproved pairing requests expire after `pairing_ttl_seconds` (default: 300). Adjust in config if your approval workflow is slower.

## Request/Response Format

### Common Headers

| Header | Required | Description |
|--------|----------|-------------|
| `Authorization` | Yes* | `Bearer <token>` (* not required in `open` mode) |
| `Content-Type` | For POST/PUT | `application/json` |
| `X-Request-Id` | No | Client-generated request ID for tracing |

### Error Format

All errors return a consistent JSON structure:

```json
{
  "error": {
    "code": "NOT_FOUND",
    "message": "Session 'ses_xyz' does not exist",
    "details": {},
    "request_id": "req_abc123"
  }
}
```

| HTTP Status | Error Code | Meaning |
|-------------|-----------|---------|
| 400 | `BAD_REQUEST` | Malformed request body or parameters |
| 401 | `UNAUTHORIZED` | Missing or invalid token |
| 403 | `FORBIDDEN` | Token lacks required permission |
| 404 | `NOT_FOUND` | Resource does not exist |
| 409 | `CONFLICT` | Resource state conflict (idempotency key reused with different content) |
| 429 | `RATE_LIMITED` | Too many requests |
| 500 | `INTERNAL_ERROR` | Unexpected server error |
| 503 | `UNAVAILABLE` | Agent is shutting down or not ready |

### Idempotent retries

Message-submitting entry points accept an idempotency key, so a retry after a
timeout or a dropped connection cannot cause the same message to be processed
twice.

| Entry point | How to pass the key |
|-------------|---------------------|
| `POST {api_prefix}/message` | `Idempotency-Key` or `X-Idempotency-Key` header |
| Webhook channel | Either header above, or an `idempotency_key` body field |
| WebSocket `message` frame | An `idempotency_key` field in the frame |

Keys must be non-empty, at most **200** characters, and free of control
characters. Supplying both a header and a body key with different values
returns 400.

```bash
curl -X POST http://127.0.0.1:58123/api/v1/message \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: order-2026-0829-001" \
  -d '{"platform":"api","user_id":"u1","chat_id":"c1","text":"build the report"}'
```

Same key with the same content replays the original event and response without
publishing again:

```json
{"status": "accepted", "event_id": "38919935...", "session_key": "gateway:api:c1"}
```

Same key with **different** content is rejected, and no new event is created:

```json
{"error": "idempotency key was already used for a different request"}
```

!!! warning "409 means key conflict, nothing else"
    409 is reserved for "this key was already used for different content". Do
    **not** retry it — use a new key, or restore the original content. An
    unfinished turn (`incomplete` / `interrupted`) returns 200 instead, with the
    nuance carried by the body's `status` field, and those requests are
    retryable.

A key's scope includes the caller's identity, so keys from different tokens
never collide:

| Entry point | Scope |
|-------------|-------|
| HTTP | token-derived principal + `session_key` |
| WebSocket | same, taken from the handshake identity |
| Webhook | `sender_id` + `chat_id` |

| Parameter | Value |
|-----------|-------|
| Record lifetime | 3600 seconds (1 hour) |
| In-process cache entries | 4096 (Gateway) / 2048 (Webhook) |
| Persisted record ceiling | 100000 |

Records are also written to SQLite, so **a retry that crosses a process restart
is still deduplicated and can replay the stored result**, independent of
per-session turn pruning. If storage is unavailable, or the unexpired-record
ceiling is reached, these endpoints **fail closed** with 503 rather than
admitting a request that might execute twice.

With `wait=false` (the default) the cached value is the delivery
acknowledgement; with `wait=true` it is the turn's final result. A retry that
races a still-running first request waits for that same result instead of
starting new work, and returns 504 on timeout.

### Pagination

List endpoints support cursor-based pagination:

```
GET /api/sessions?limit=20&cursor=cur_abc123
```

Response includes pagination metadata:

```json
{
  "items": [...],
  "pagination": {
    "cursor": "cur_def456",
    "has_more": true,
    "total": 142
  }
}
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | int | 20 | Items per page (max 100) |
| `cursor` | string | — | Cursor from previous response |

## Rate Limiting

Rate limits are enforced per-token when `security.profile` is `standard` or `extended`:

| Endpoint Group | Limit | Window |
|---------------|-------|--------|
| Read operations | 120 req | 1 minute |
| Write operations | 30 req | 1 minute |
| Analytics | 10 req | 1 minute |
| Lifecycle | 5 req | 1 minute |

Rate limit headers are included in every response:

```
X-RateLimit-Limit: 120
X-RateLimit-Remaining: 117
X-RateLimit-Reset: 1724072460
```

---

## Endpoints

### `/api/sessions`

Manage agent conversation sessions.

#### List Sessions

```
GET /api/sessions
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `channel` | string | Filter by channel name |
| `status` | string | Filter: `active`, `archived`, `all` |
| `limit` | int | Page size |
| `cursor` | string | Pagination cursor |

Response:

```json
{
  "items": [
    {
      "id": "ses_a1b2c3",
      "channel": "telegram",
      "created_at": "2026-08-18T09:00:00Z",
      "last_active": "2026-08-19T14:30:00Z",
      "message_count": 47,
      "status": "active"
    }
  ],
  "pagination": {"cursor": "cur_...", "has_more": false, "total": 3}
}
```

#### Get Session

```
GET /api/sessions/{session_id}
```

#### Create Session

```
POST /api/sessions
Content-Type: application/json

{
  "channel": "api",
  "metadata": {"purpose": "automated-test"}
}
```

#### Send Message to Session

```
POST /api/sessions/{session_id}/messages
Content-Type: application/json

{
  "content": "Summarize yesterday's logs",
  "role": "user"
}
```

Response (streaming available via `Accept: text/event-stream`):

```json
{
  "id": "msg_x1y2z3",
  "role": "assistant",
  "content": "Yesterday's logs show 3 warnings...",
  "tool_calls": [],
  "cost": {"input_tokens": 1200, "output_tokens": 340, "total_usd": 0.0082}
}
```

#### Delete Session

```
DELETE /api/sessions/{session_id}
```

!!! warning "Irreversible"
    Deleting a session removes all messages and associated context. Memory entries created during the session are preserved.

#### Durable Turn Status

```
GET /api/v1/sessions/{key}/turns?limit=20
GET /api/v1/turns/{event_id}
```

These admin-scoped endpoints expose the authoritative lifecycle ledger used by
CLI reconnect reconciliation. States are `accepted`, `running`,
`waiting_approval`, `waiting_clarification`, `completed`, `incomplete`,
`failed`, or `interrupted`. A record also carries its current tool, response,
termination reason, context epoch, and timestamps. The list limit must be
between 1 and 100; a runtime without durable storage returns `503`.

---

### `/api/memory`

Access and manage the agent's persistent memory.

#### List Memories

```
GET /api/memory
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `query` | string | Semantic search query |
| `tag` | string | Filter by tag |
| `since` | ISO datetime | Only memories after this timestamp |
| `limit` | int | Page size |

Response:

```json
{
  "items": [
    {
      "id": "mem_abc",
      "content": "User prefers YAML over JSON for config files",
      "tags": ["preference"],
      "created_at": "2026-08-10T08:00:00Z",
      "last_accessed": "2026-08-19T10:00:00Z",
      "relevance_score": 0.92
    }
  ]
}
```

#### Create Memory

```
POST /api/memory
Content-Type: application/json

{
  "content": "Project uses PostgreSQL 16 in production",
  "tags": ["infrastructure", "database"]
}
```

#### Update Memory

```
PUT /api/memory/{memory_id}
Content-Type: application/json

{
  "content": "Project migrated to PostgreSQL 17 in production",
  "tags": ["infrastructure", "database"]
}
```

#### Delete Memory

```
DELETE /api/memory/{memory_id}
```

---

### `/api/knowledge`

Manage the agent's knowledge base (document embeddings and retrieval).

#### List Knowledge Entries

```
GET /api/knowledge
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `query` | string | Semantic search across knowledge base |
| `source` | string | Filter by source (file path, URL) |
| `limit` | int | Page size |

Response:

```json
{
  "items": [
    {
      "id": "know_x1",
      "title": "Deployment Runbook",
      "source": "/docs/runbook.md",
      "chunk_count": 12,
      "indexed_at": "2026-08-15T10:00:00Z",
      "size_bytes": 24576
    }
  ]
}
```

#### Add Knowledge

```
POST /api/knowledge
Content-Type: application/json

{
  "title": "API Design Guidelines",
  "content": "All endpoints must use...",
  "source": "manual",
  "metadata": {"author": "team-lead"}
}
```

#### Delete Knowledge Entry

```
DELETE /api/knowledge/{knowledge_id}
```

#### Re-index Knowledge

```
POST /api/knowledge/{knowledge_id}/reindex
```

---

### `/api/skills`

Manage the agent's skill registry.

#### List Skills

```
GET /api/skills
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `status` | string | `active`, `staged`, `disabled` |
| `category` | string | Filter by category |

Response:

```json
{
  "items": [
    {
      "id": "skill_deploy",
      "name": "deploy-to-staging",
      "description": "Deploys current branch to staging environment",
      "status": "active",
      "version": 3,
      "last_used": "2026-08-19T11:00:00Z",
      "use_count": 28,
      "evolved_from": "skill_deploy_v2"
    }
  ]
}
```

#### Get Skill Details

```
GET /api/skills/{skill_id}
```

#### Enable/Disable Skill

```
PUT /api/skills/{skill_id}/status
Content-Type: application/json

{"status": "disabled"}
```

#### Approve Staged Skill

```
POST /api/skills/{skill_id}/approve
```

!!! tip "Skill evolution"
    Skills flagged as `staged` were auto-generated by the evolution system. They require explicit approval before activation. Use the TUI command `/approve` or this endpoint.

---

### `/api/tasks`

Manage the agent's task queue.

#### List Tasks

```
GET /api/tasks
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `status` | string | `pending`, `running`, `completed`, `failed` |
| `priority` | string | `low`, `normal`, `high`, `urgent` |
| `limit` | int | Page size |

Response:

```json
{
  "items": [
    {
      "id": "task_r2d2",
      "title": "Generate weekly report",
      "status": "running",
      "priority": "normal",
      "created_at": "2026-08-19T08:00:00Z",
      "started_at": "2026-08-19T08:01:00Z",
      "progress": 0.65
    }
  ]
}
```

#### Create Task

Requires an admin-scoped token. Returns `201` with `{"task": {...}}`.

```
POST /api/tasks
Content-Type: application/json

{
  "title": "Analyze error logs from last 24h",
  "description": "Group by service, summarise the top offenders",
  "priority": 5,
  "labels": ["ops"],
  "assignee": "",
  "source": "human",
  "board_id": "default",
  "parent_task_id": "",
  "metadata": {}
}
```

Only `title` is required — an empty one returns `400`. `priority` must be an
integer (default `5`; booleans are rejected), `labels` an array of strings, and
`metadata` an object — otherwise the request is rejected with `400`. Use
`parent_task_id` to nest the task under an existing one.

The same field validation applies to `PATCH /api/tasks/{id}`, which checks only
the fields actually present in the request.

#### Cancel Task

```
DELETE /api/tasks/{task_id}
```

---

### `/api/cron`

Manage scheduled jobs. Requires admin token.

#### List Cron Jobs

```
GET /api/cron
```

Response:

```json
{
  "items": [
    {
      "id": "cron_daily_summary",
      "schedule": "0 9 * * *",
      "prompt": "Summarize overnight alerts and create a morning brief",
      "enabled": true,
      "authorized": true,
      "last_run": "2026-08-19T09:00:00Z",
      "next_run": "2026-08-20T09:00:00Z"
    }
  ]
}
```

#### Create Cron Job

```
POST /api/cron
Content-Type: application/json

{
  "schedule": "*/30 * * * *",
  "prompt": "Check deployment health and notify if degraded",
  "enabled": true
}
```

#### Authorize Cron Job

```
POST /api/cron/{cron_id}/authorize
```

#### Revoke Cron Authorization

```
POST /api/cron/{cron_id}/revoke
```

#### Delete Cron Job

```
DELETE /api/cron/{cron_id}
```

!!! warning "Authorization required"
    In `standard` and `extended` security profiles, cron jobs must be explicitly authorized before they will execute. Unauthorized jobs remain in the schedule but are skipped at runtime.

---

### `/api/channels`

View and manage communication channel status. Requires admin token for modifications.

#### List Channels

```
GET /api/channels
```

Response:

```json
{
  "items": [
    {
      "name": "telegram",
      "enabled": true,
      "connected": true,
      "uptime_seconds": 86420,
      "messages_today": 34,
      "last_message_at": "2026-08-19T14:22:00Z"
    },
    {
      "name": "discord",
      "enabled": true,
      "connected": false,
      "error": "Token expired",
      "last_connected_at": "2026-08-18T23:00:00Z"
    }
  ]
}
```

#### Reconnect Channel

```
POST /api/channels/{channel_name}/reconnect
```

#### Disable Channel

```
POST /api/channels/{channel_name}/disable
```

---

### `/api/analytics`

Usage and cost analytics.

#### Get Cost Summary

```
GET /api/analytics/cost
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `period` | string | `today`, `week`, `month`, `all` |
| `group_by` | string | `model`, `channel`, `session`, `day` |

Response:

```json
{
  "period": "today",
  "total_usd": 1.47,
  "breakdown": [
    {"model": "claude-sonnet-5", "input_tokens": 52000, "output_tokens": 18000, "cost_usd": 0.89},
    {"model": "claude-haiku-4-5", "input_tokens": 120000, "output_tokens": 45000, "cost_usd": 0.58}
  ],
  "daily_trend": [
    {"date": "2026-08-19", "cost_usd": 1.47},
    {"date": "2026-08-18", "cost_usd": 2.10}
  ]
}
```

#### Get Usage Statistics

```
GET /api/analytics/usage
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `period` | string | `today`, `week`, `month` |

Response:

```json
{
  "period": "today",
  "sessions": 5,
  "messages": 87,
  "tool_calls": 142,
  "tasks_completed": 3,
  "skills_used": 8
}
```

---

### `/api/config`

Runtime configuration management. Read requires API token; write requires admin token.

#### Get Current Config

```
GET /api/config
```

Returns the merged, active configuration (secrets redacted):

```json
{
  "security": {"profile": "standard"},
  "models": {"default": "claude-sonnet-5", "planning": "claude-opus-5"},
  "tools": {"profile": "coding"},
  "..."
}
```

#### Update Config

```
PUT /api/config
Content-Type: application/json

{
  "models": {"default": "claude-haiku-4-5"}
}
```

!!! warning "Partial updates"
    PUT performs a deep merge—only specified fields are changed. To reset a field to its default, set it to `null`.

#### Validate Config

```
POST /api/config/validate
Content-Type: application/json

{
  "tools": {"profile": "invalid_value"}
}
```

Response (validation failure):

```json
{
  "valid": false,
  "errors": [
    {"path": "tools.profile", "message": "Must be one of: minimal, messaging, coding, full"}
  ]
}
```

---

### `/api/logs`

Retrieve agent logs.

#### Get Logs

```
GET /api/logs
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `level` | string | Minimum level: `debug`, `info`, `warning`, `error` |
| `since` | ISO datetime | Logs after this timestamp |
| `until` | ISO datetime | Logs before this timestamp |
| `source` | string | Filter by component (e.g., `gateway`, `channel.telegram`) |
| `limit` | int | Max entries (default 100, max 1000) |

Response:

```json
{
  "items": [
    {
      "timestamp": "2026-08-19T14:30:01.234Z",
      "level": "info",
      "source": "gateway",
      "message": "Session ses_a1b2c3 created via API",
      "metadata": {"client_ip": "127.0.0.1"}
    }
  ]
}
```

---

### Health check

#### Health Check

```
GET {api_prefix}/health
```

The default path is `/api/v1/health`. No authentication is required. Returns:

```json
{
  "status": "healthy",
  "server_running": true,
  "active_channels": {"telegram": "active"},
  "ws_clients": 1,
  "provider": "ok",
  "media_cache_mb": 2.4,
  "active_sessions": 1,
  "total_sessions": 8
}
```

| Status | Meaning |
|--------|---------|
| `healthy` | All systems operational |
| `degraded` | Some non-critical components have issues |
| `unhealthy` | Critical component failure |

---

## Origin and Host Protection

When `security.profile` is `standard` or `extended`, the gateway validates request origins:

```yaml
gateway:
  auth:
    allowed_origins:
      - "http://localhost:3000"
      - "https://my-dashboard.example.com"
    allowed_hosts:
      - "localhost:3007"
      - "echo.internal:3007"
```

Requests with non-matching `Origin` or `Host` headers receive `403 FORBIDDEN`.

## Configuration Reference

```yaml
gateway:
  host: "127.0.0.1"
  port: 3007
  auth:
    mode: allowlist              # open | allowlist | pairing
    api_tokens: []               # list of valid API tokens
    admin_tokens: []             # list of admin tokens (superset of API)
    allowed_origins: []          # Origin header allowlist
    allowed_hosts: []            # Host header allowlist
    allowed_users: []            # user identifiers allowed access
    admin_users: []              # users with admin privileges
    token_header: "Authorization" # header name for token
    pairing_ttl_seconds: 300     # pairing request expiry
```

!!! note "Auth changes require a restart"
    `/api/config` is read-only — only `GET /api/config` is registered, with no write counterpart. There is no live-reload path for authentication settings.

    Editing `gateway.auth` (adding a token, extending `allowed_users`) therefore means changing the configuration file and running `echo-agent gateway restart`. Tokens issued before the restart keep working until then; ones added after it take effect only once the process has restarted.
