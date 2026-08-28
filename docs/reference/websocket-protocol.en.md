# WebSocket Protocol Reference

Echo Agent Gateway exposes one WebSocket for agent sessions and one for Dashboard events.

| Endpoint | Default path | Purpose |
|----------|--------------|---------|
| Session | `/ws` | Text interaction with the agent |
| Dashboard | `/ws/dashboard` | Subscribe to runtime events such as tasks and cron jobs |

The session path is configurable through `gateway.ws_path`. This page documents only frames accepted or emitted by the current server. Skill, knowledge, and other management operations use the REST API.

## Session WebSocket

### Connection and authentication

After the connection is upgraded, send an `auth` frame within five seconds. The token may be carried by that frame, an upgrade-request header, or the URL query, but the `auth` frame itself is always required.

```json
{
  "type": "auth",
  "token": "your-api-token",
  "platform": "cli",
  "user_id": "alice",
  "chat_id": "alice",
  "session_key": "cli:alice"
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | Must be `auth` |
| `token` | By configuration | API or admin token; omit when no tokens are configured |
| `platform` | No | Client platform; missing or unknown values normalize to `ws` |
| `user_id` | By auth mode | User identity |
| `chat_id` | No | Delivery identity; defaults to `user_id` |
| `session_key` | No | Resume a session; ownership is validated by the server |

An upgrade request may instead carry:

```http
Authorization: Bearer your-api-token
```

The header configured by `gateway.auth.token_header` is also accepted. The compatibility form `?token=` still authenticates, but query values can be retained in access logs, proxy logs, and browser history. Prefer a header or the `auth` frame in production.

Successful authentication returns:

```json
{"type": "auth_ok", "session_key": "cli:alice"}
```

On failure the server sends an error and closes the socket:

```json
{"type": "error", "error": "unauthorized"}
```

### Client frames

#### message

Send a plain-text message:

```json
{
  "type": "message",
  "text": "Summarize recent task progress",
  "is_group": false
}
```

Once queued, the server acknowledges it with an event ID:

```json
{"type": "accepted", "event_id": "evt_abc123"}
```

`accepted` means queued, not completed. Empty text is ignored.

#### interrupt

Request cooperative cancellation of the active turn. Include the `event_id` from its `accepted` frame when possible so a delayed interrupt cannot affect the next turn.

```json
{"type": "interrupt", "event_id": "evt_abc123"}
```

When queued successfully, the server returns `{"type":"accepted"}`.

#### ping

```json
{"type": "ping"}
```

The server returns `{"type":"pong"}`.

### Server message frames

Agent output uses a common `message` frame. Stream fragments and completed messages are distinguished by `is_final`; `message_kind` and `metadata` carry generic progress, cognitive-state, and error information.

```json
{
  "type": "message",
  "event_id": "evt_reply",
  "reply_to_id": "evt_abc123",
  "channel": "gateway:cli",
  "chat_id": "alice",
  "text": "Three tasks are complete.",
  "is_final": true,
  "message_kind": "final",
  "edit_message_id": null,
  "metadata": {}
}
```

Common error frames are:

```json
{"type": "error", "error": "authenticate first"}
{"type": "error", "error": "rate limited"}
{"type": "error", "error": "server overloaded"}
{"type": "error", "error": "internal error"}
```

## Dashboard WebSocket

After connecting to `/ws/dashboard`, send an auth frame within five seconds. This endpoint currently reads its token from the frame itself:

```json
{"type": "auth", "token": "your-api-token"}
```

The server responds with `{"type":"auth_ok"}`. Subscribe to event channels afterward:

```json
{"type": "subscribe", "channels": ["tasks", "cron"]}
```

```json
{"type": "subscribed", "channels": ["cron", "tasks"]}
```

To unsubscribe:

```json
{"type": "unsubscribe", "channels": ["cron"]}
```

`tasks` and `cron` currently have live event sources. `sessions`, `memory`, `skills`, `channels`, `logs`, `analytics`, and `knowledge` are reserved subscription names without connected emitters. Unknown names produce `subscribe_error`.

Broadcasts use a common envelope:

```json
{
  "type": "task_created",
  "payload": {
    "id": "task_123",
    "status": "completed"
  }
}
```

## Connection maintenance

- The server sends WebSocket control-frame heartbeats at `gateway.ws_heartbeat_seconds`, 30 seconds by default.
- Clients should reconnect with exponential backoff and jitter.
- Reauthenticate with the original `session_key` to continue using that session. The server has no message-replay protocol.
- Reverse-proxy idle timeouts should exceed the heartbeat interval.
