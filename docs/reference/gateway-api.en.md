# Gateway API Reference

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

The Gateway listens at `http://127.0.0.1:58123` by default, with the configurable `/api/v1` API prefix (`gateway.api_prefix`). HTTP routes are registered in `echo_agent/gateway/server.py` and `echo_agent/gateway/api/__init__.py`. All paths below are relative to the prefix. The server also registers `/meta`, `/playground`, `/ws`, and `/ws/dashboard`; the main WebSocket path is configurable.

## Authentication

The default token header is `X-Echo-Agent-Token`, configurable through `gateway.auth.token_header`. `gateway.auth.mode` controls open, allowlist, or pairing authorization for platform/user identities; API tokens and user pairing are separate checks. When `gateway.auth.api_tokens` is configured, read and message endpoints require a valid token. Management endpoints require `gateway.auth.admin_tokens`, falling back to `api_tokens` when no admin tokens are configured. Management endpoints read tokens from headers, not query parameters. The health endpoint requires no token. See [Gateway authentication](../integrations/gateway/authentication.en.md).

`POST /api/v1/pair` requires an API token. `POST /api/v1/pair/verify` accepts `platform`, `user_id`, and `code`. Verification authorizes that platform/user combination; it does not issue a new API token. Codes expire after 300 seconds by default.

## Registered endpoints

| Method | Path (append to `/api/v1`) | Purpose |
|---|---|---|
| `POST` | `/message` | Submit a message |
| `GET` | `/health` | Health status |
| `GET` | `/ready` | Local task-admission readiness, without remote model probes |
| `GET` | `/stats` | Runtime statistics |
| `GET` | `/capabilities` | Caller capabilities |
| `POST` | `/pair` | Generate a pairing code |
| `POST` | `/pair/verify` | Verify a pairing code |
| `GET` | `/sessions` | List sessions |
| `DELETE` | `/sessions/{key}` | Reset a session |
| `GET` | `/sessions/{key}/history` | Session history |
| `GET` | `/sessions/{key}/turns` | Session turn states |
| `GET` | `/turns/{event_id}` | Get a turn by event ID |
| `GET` | `/memory` | List memory entries |
| `GET` | `/memory/stats` | Memory statistics |
| `POST` | `/memory/search` | Search memory |
| `GET` | `/memory/{id}` | Get a memory entry |
| `PUT` | `/memory/{id}` | Update a memory entry |
| `DELETE` | `/memory/{id}` | Delete a memory entry |
| `GET` | `/skills` | List skills |
| `POST` | `/skills/import` | Import a skill |
| `POST` | `/skills/upload` | Upload a skill |
| `GET` | `/skills/{name}` | Get a skill |
| `GET` | `/skills/{name}/deps` | Get skill dependencies |
| `POST` | `/skills/{name}/deps/install` | Install skill dependencies |
| `POST` | `/skills/{name}/toggle` | Toggle a skill |
| `DELETE` | `/skills/{name}` | Delete a skill |
| `GET` | `/channels` | List channels |
| `POST` | `/channels/{name}/{action}` | Channel lifecycle action |
| `GET` | `/knowledge/status` | Knowledge-base status |
| `POST` | `/knowledge/rebuild` | Rebuild the index |
| `POST` | `/knowledge/upload` | Upload a document |
| `GET` | `/knowledge/documents` | List documents |
| `DELETE` | `/knowledge/documents/{path}` | Delete a document (nested paths supported) |
| `GET` | `/knowledge/jobs` | List indexing jobs |
| `GET` | `/knowledge/jobs/{id}` | Get an indexing job |
| `DELETE` | `/knowledge/jobs/{id}` | Cancel an indexing job |
| `GET` | `/config` | Read redacted configuration |
| `PATCH` | `/config` | Update configuration |
| `GET` | `/tasks` | List tasks |
| `POST` | `/tasks` | Create a task |
| `GET` | `/tasks/{id}` | Get a task |
| `PUT` | `/tasks/{id}` | Update a task |
| `DELETE` | `/tasks/{id}` | Delete a task |
| `POST` | `/tasks/{id}/transition` | Transition a task |
| `POST` | `/tasks/{id}/retry` | Retry a task |
| `GET` | `/cron` | List cron jobs |
| `POST` | `/cron` | Create a cron job |
| `PUT` | `/cron/{id}` | Update a cron job |
| `DELETE` | `/cron/{id}` | Delete a cron job |
| `POST` | `/cron/{id}/trigger` | Trigger a cron job |
| `GET` | `/cron/{id}/runs` | Cron run history |
| `GET` | `/logs` | Query logs |
| `GET` | `/analytics/tokens` | Token usage |
| `GET` | `/analytics/skills` | Skill usage |
| `GET` | `/analytics/channels` | Channel usage |

`GET /sessions` filters with `channel` and `q`. Supplying `limit` or `offset` enables offset pagination; `limit` is 1–500 and defaults to 100. `GET /sessions/{key}/history` accepts `limit` (1–500) and `offset`; `total` counts all visible history and `returned` counts this page. `GET /sessions/{key}/turns` accepts `limit` from 1 to 100, defaulting to 20. Other list handlers define their own parameters and response shapes; there is no universal `data/meta` envelope or cursor pagination protocol.

`POST /message` accepts fields including `platform`, `user_id`, `chat_id`, and `text`. For example:

```bash
curl -X POST http://127.0.0.1:58123/api/v1/message \
  -H 'Content-Type: application/json' \
  -H 'X-Echo-Agent-Token: YOUR_TOKEN' \
  -d '{"platform":"api","user_id":"u1","chat_id":"c1","text":"build the report"}'
```

Error responses generally use `{"error": "..."}`; exact fields and status codes depend on the endpoint. `GET {api_prefix}/health` (default `/api/v1/health`) returns 200 or 503 according to health status.

`GET {api_prefix}/ready` requires no token. It returns 200 only when Gateway, bus, Agent, SQLite connection, a non-Stub provider configuration and required runtime dependencies are locally ready; otherwise it returns 503 with individual `checks`. `provider_remote_verified: false` means no remote probe was performed: 200 does not verify model credentials, quota or upstream networking. A missing model can leave the configuration UI available while readiness remains false.

`GET /config` returns redacted saved values, with active values under `_meta.running` and differences under `_meta.pending_paths`/`_meta.restart_required`. `PATCH /config` validates non-secret leaf fields and saves atomically without mutating the constructed Runtime. Nested patches merge and validate leaves. Explicit channel start/stop/restart loads that channel's saved settings. Environment-variable precedence still applies when reloading.

`POST /cron/{id}/trigger` returns 404 for an absent job and actionable 409 conflicts for disabled/paused/terminal jobs, active runs, full execution capacity or run-lock conflicts.

## Idempotent retries

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
    unfinished turn (`incomplete` / `interrupted`) can return 200 with the outcome
    in `status`. The same key only replays the saved terminal result; it does
    not create a new turn. Check existing results and external side effects
    before explicitly submitting new work. This does not promise exactly-once
    execution for arbitrary external effects.

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



## Pagination

Pagination is endpoint-specific. `GET /sessions` and `GET /sessions/{key}/history` accept `limit` and `offset`; `GET /memory` and `GET /logs` also support offset pagination. `GET /sessions/{key}/turns` only accepts `limit`. These endpoints do not accept a `cursor`.
