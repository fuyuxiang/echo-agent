# WebSocket Protocol Reference

Echo Agent exposes two WebSocket endpoints for real-time communication with the agent runtime and monitoring dashboard.

| Endpoint | Purpose | Auth Required |
|----------|---------|---------------|
| `/ws/session` | Bidirectional chat with the agent | API token |
| `/ws/dashboard` | Live monitoring updates | Admin token |

---

## Connection

### URL Format

```
ws://localhost:3000/ws/session?token=<api_token>
ws://localhost:3000/ws/dashboard?token=<admin_token>
```

### Authentication

Tokens can be provided via:

1. **Query parameter**: `?token=<value>`
2. **Header**: The configured `token_header` (default: `X-Echo-Agent-Token`), or `Authorization: Bearer <value>`
3. **Auth frame**: a `{"type": "auth", "token": "<value>"}` first frame

### Token source and scope

All three sources complete the handshake and grant api scope, but **only headers and
the auth frame grant admin scope**:

| Source | Handshake | Read-only frames | State-changing frames |
|--------|-----------|------------------|-----------------------|
| Header | ✅ | ✅ | ✅ |
| Auth frame | ✅ | ✅ | ✅ |
| URL `?token=` | ✅ | ✅ | ❌ |

A `?token=` value is recorded by aiohttp's default access log along with the query
string, and also lands in reverse-proxy logs, browser history and referrers — the token
outlives its own useful life in those logs. So no state-changing frame (for example
`skill.enable` or `skill.disable`) accepts a URL-borne token. This matches the rule the
HTTP admin endpoints already enforce.

This holds **regardless of whether `admin_tokens` is configured**. In a single-token
deployment that sets only `api_tokens`, the api token acts as admin by fallback and is
subject to the same restriction.

!!! warning "URL auth cannot perform writes"
    A client connected with `?token=` that sends a state-changing frame receives an
    `admin token required` error. Pass the token in a header or the auth frame instead.
    Read-only frames are unaffected.

### Connection Lifecycle

```
Client                          Server
  |                               |
  |--- WebSocket Upgrade -------->|
  |    (with token)               |
  |                               |
  |<-- 101 Switching Protocols ---|
  |                               |
  |<-- status (connected) --------|
  |                               |
  |--- chat message ------------->|
  |<-- stream_start --------------|
  |<-- stream_chunk (n times) ----|
  |<-- stream_end ----------------|
  |                               |
  |--- ping --------------------->|
  |<-- pong ----------------------|
  |                               |
  |--- close -------------------->|
  |<-- close ---------------------|
```

---

## /ws/session Protocol

### Client-to-Server Messages

All messages are JSON frames with a `type` field.

| Type | Description | Required Fields |
|------|-------------|-----------------|
| `chat` | Send a message to the agent | `content` |
| `approve` | Approve a pending tool execution | `approval_id` |
| `deny` | Deny a pending tool execution | `approval_id`, `reason` (optional) |
| `clarify` | Respond to a clarification request | `clarify_id`, `content` |
| `cancel` | Cancel the current operation | — |

#### chat

```json
{
  "type": "chat",
  "content": "What files changed in the last commit?",
  "session_id": "sess_abc123",
  "metadata": {
    "channel": "web"
  }
}
```

#### approve

```json
{
  "type": "approve",
  "approval_id": "appr_def456"
}
```

#### deny

```json
{
  "type": "deny",
  "approval_id": "appr_def456",
  "reason": "Too risky, try a read-only approach"
}
```

#### clarify

```json
{
  "type": "clarify",
  "clarify_id": "clar_ghi789",
  "content": "Use the production database"
}
```

#### cancel

```json
{
  "type": "cancel"
}
```

### Server-to-Client Messages

| Type | Description | Key Fields |
|------|-------------|------------|
| `message` | Complete agent response | `content`, `session_id` |
| `tool_call` | Agent is invoking a tool | `tool`, `args`, `call_id` |
| `tool_result` | Tool execution result | `call_id`, `result`, `success` |
| `approval_request` | Tool needs user approval | `approval_id`, `tool`, `args`, `risk` |
| `error` | Error occurred | `code`, `message` |
| `status` | Connection/session status | `state`, `session_id` |
| `stream_start` | Beginning of streamed response | `stream_id` |
| `stream_chunk` | Partial response content | `stream_id`, `delta` |
| `stream_end` | End of streamed response | `stream_id`, `content` |

#### message

```json
{
  "type": "message",
  "content": "The last commit modified 3 files...",
  "session_id": "sess_abc123",
  "metadata": {
    "tokens_in": 245,
    "tokens_out": 89,
    "cost": 0.0034,
    "model": "claude-sonnet-4-20250514",
    "latency_ms": 1820
  }
}
```

#### tool_call

```json
{
  "type": "tool_call",
  "call_id": "tc_001",
  "tool": "shell",
  "args": {
    "command": "git log --oneline -1 --stat"
  }
}
```

#### tool_result

```json
{
  "type": "tool_result",
  "call_id": "tc_001",
  "success": true,
  "result": "a6c96e7 Fix channel system...\n 3 files changed, 45 insertions(+), 12 deletions(-)"
}
```

#### approval_request

```json
{
  "type": "approval_request",
  "approval_id": "appr_def456",
  "tool": "shell",
  "args": {
    "command": "rm -rf /tmp/build-cache"
  },
  "risk": "high",
  "reason": "Destructive shell command requires approval"
}
```

#### error

```json
{
  "type": "error",
  "code": "SESSION_EXPIRED",
  "message": "Session has expired after 30 minutes of inactivity"
}
```

#### stream_start / stream_chunk / stream_end

```json
{"type": "stream_start", "stream_id": "str_001"}
{"type": "stream_chunk", "stream_id": "str_001", "delta": "The last "}
{"type": "stream_chunk", "stream_id": "str_001", "delta": "commit modified "}
{"type": "stream_chunk", "stream_id": "str_001", "delta": "3 files."}
{"type": "stream_end", "stream_id": "str_001", "content": "The last commit modified 3 files."}
```

---

## /ws/dashboard Protocol

The dashboard WebSocket is server-push only. Clients do not send messages after connection (except ping frames).

### Event Types

| Event | Description | Payload |
|-------|-------------|---------|
| `session_update` | Session state changed | `session_id`, `state`, `channel` |
| `cost_update` | Cost metrics updated | `total`, `period`, `breakdown` |
| `task_update` | Task queue changed | `task_id`, `status`, `progress` |
| `channel_status` | Channel connectivity changed | `channel`, `status`, `error` |
| `memory_event` | Memory written or recalled | `operation`, `key`, `preview` |
| `skill_event` | Skill evolved or staged | `skill`, `action`, `version` |

#### session_update

```json
{
  "type": "session_update",
  "session_id": "sess_abc123",
  "state": "active",
  "channel": "telegram",
  "user": "user_123",
  "started_at": "2025-01-15T10:30:00Z"
}
```

#### cost_update

```json
{
  "type": "cost_update",
  "total": 12.45,
  "period": "daily",
  "breakdown": {
    "input_tokens": 1250000,
    "output_tokens": 380000,
    "tool_calls": 245
  }
}
```

#### task_update

```json
{
  "type": "task_update",
  "task_id": "task_xyz",
  "status": "completed",
  "progress": 100,
  "result_preview": "Report generated successfully"
}
```

#### channel_status

```json
{
  "type": "channel_status",
  "channel": "telegram",
  "status": "connected",
  "error": null,
  "uptime_seconds": 86400
}
```

#### memory_event

```json
{
  "type": "memory_event",
  "operation": "write",
  "key": "user_preference_theme",
  "preview": "User prefers dark mode for all interfaces"
}
```

#### skill_event

```json
{
  "type": "skill_event",
  "skill": "code_review",
  "action": "promoted",
  "version": "1.3.0",
  "improvement": "+12% accuracy on eval set"
}
```

### Subscription Model

By default, all event types are delivered. Clients can filter by sending a subscription message immediately after connection:

```json
{
  "type": "subscribe",
  "events": ["session_update", "cost_update"]
}
```

---

## Heartbeat

Both endpoints support WebSocket ping/pong frames for connection health.

| Parameter | Default | Description |
|-----------|---------|-------------|
| Ping interval | 30s | Server sends ping every 30 seconds |
| Pong timeout | 10s | Connection closed if pong not received within 10s |
| Client ping | Optional | Clients may send ping at any time |

!!! tip "Proxy configuration"
    If using a reverse proxy (nginx, Caddy), ensure WebSocket timeouts exceed the ping interval. Set `proxy_read_timeout 60s` or equivalent.

---

## Reconnection

### Strategy

Clients should implement exponential backoff with jitter:

```python
import random
import asyncio

async def connect_with_retry(url, max_retries=10):
    for attempt in range(max_retries):
        try:
            return await websocket_connect(url)
        except ConnectionError:
            delay = min(30, (2 ** attempt)) + random.uniform(0, 1)
            await asyncio.sleep(delay)
    raise MaxRetriesExceeded()
```

### Session Resumption

When reconnecting to `/ws/session`, include the last known `session_id` to resume:

```json
{
  "type": "resume",
  "session_id": "sess_abc123",
  "last_message_id": "msg_042"
}
```

The server will replay any messages sent after `last_message_id`.

!!! warning "Resumption window"
    Session state is retained for the duration configured in `session.timeout` (default: 30 minutes). After expiry, a new session is created.

---

## Error Codes

| Code | HTTP Equiv | Description | Recovery |
|------|-----------|-------------|----------|
| `AUTH_FAILED` | 401 | Invalid or missing token | Provide valid token |
| `AUTH_EXPIRED` | 401 | Token has expired | Refresh token and reconnect |
| `FORBIDDEN` | 403 | Insufficient permissions | Use admin token for dashboard |
| `SESSION_EXPIRED` | 410 | Session no longer exists | Start new session |
| `RATE_LIMITED` | 429 | Too many messages | Back off and retry |
| `INTERNAL_ERROR` | 500 | Server-side failure | Retry with backoff |
| `OVERLOADED` | 503 | Server at capacity | Retry later |
| `INVALID_MESSAGE` | 400 | Malformed JSON or unknown type | Fix message format |
| `TOOL_NOT_FOUND` | 404 | Referenced tool does not exist | Check tool name |
| `APPROVAL_EXPIRED` | 410 | Approval request timed out | Agent will re-request if needed |

---

## Best Practices

!!! tip "Client implementation checklist"
    - Always handle `error` messages and log them
    - Implement reconnection with exponential backoff
    - Buffer user input during reconnection
    - Display `approval_request` prominently — they block agent progress
    - Use `stream_chunk` for real-time display, `stream_end` for final content
    - Send `cancel` if the user navigates away during a long operation

### Message Ordering

Messages are delivered in order within a single WebSocket connection. After reconnection with session resumption, replayed messages maintain their original order.

### Binary Data

All WebSocket frames are text (UTF-8 JSON). Binary data (images, files) is transmitted as base64-encoded strings within JSON fields or via HTTP upload with a reference URL in the message.

### Connection Limits

| Limit | Default | Configurable |
|-------|---------|--------------|
| Sessions per minute per session key | `rate_limit.session_rpm` | Yes (`rate_limit.session_rpm`) |
| Burst headroom on the rate limiter | `rate_limit.session_burst` | Yes (`rate_limit.session_burst`) |
| Idle timeout before session is reset | 24 h (`session_policy.idle_timeout_minutes`) | Yes (`gateway.session_policy.idle_timeout_minutes`) |
| Max message size, max sessions per token | — | Not configurable in the schema; the gateway imposes its own limits at the framework level |
