# Session Management

Echo Agent's session system gives each conversation key an independent model context and supports concurrent channels and sessions.

!!! warning "Session isolation is not access control"
    A session key prevents one conversation's working context from being mixed into another. It does not prove who may read that session. The Dashboard Sessions page and `session_search` intentionally inspect state across sessions. Mutually untrusted users should not share an instance based only on separate session keys or one API token each; see the [security model](../concepts/security-model.md#multi-client-tenant-boundary).

## Session Key Composition

The base key consists of a channel and the platform's chat identifier, in the form `{channel}:{chat_id}` (`InboundEvent.session_key`):

| Part | Source | Description |
|------|--------|-------------|
| `channel` | `event.channel` | Registered channel name, such as `slack`, `weixin`, or `telegram` |
| `chat_id` | `event.chat_id` | Platform conversation identifier for either a direct or group chat |

```text
slack:C67890
```

When an internal event carries `session_key_override`, that validated override is used instead of composing the base key.

### Per-user context inside a group

For group chats, `scoped_session_key(scope)` can additionally include the sender:

| `scope` | Direct chat | Group chat |
|---------|-------------|------------|
| `shared` | `slack:D1` | `slack:C67890` (one context for the group) |
| `per_user` | `slack:D1` | `slack:C67890:U12345` (one context per sender) |

`per_user` appends the sender only for a group chat with a non-empty `sender_id`. The operation is idempotent, so an existing sender suffix is not duplicated.

!!! note "Context isolation model"
    Conversations with different resulting keys do not share live message context. Whether group members share a context is controlled by `scope`; the key is a context boundary, not an authorization credential.

## Session Lifecycle

```
Creation → Active → Expired → Archived
```

### Creation

A session is automatically created when the first message arrives for a new composite key. The session dataclass contains:

- `key` — Composite key
- `messages` — Message list
- `created_at` — Creation timestamp
- `updated_at` — Last update timestamp
- `metadata` — Metadata dictionary
- `last_consolidated` — Last consolidation position
- `status` — Status (active | expired | archived)

### Active

While active, a session continues to receive and process messages. Each interaction updates the `updated_at` timestamp.

### Expired

When a session has been inactive longer than `session.expiryHours` (72 hours by default), its status transitions to `expired`.

Expiry is not terminal: as long as the session is still in the in-memory cache, the next access flips it back to `active` and refreshes `updated_at`, so the conversation continues without creating a new session. This automatic recovery only applies on a cache hit.

### Archived

An `expired` session becomes `archived` after 168 hours (7 days) of silence and is dropped from the in-memory cache, so unlike `expired` it cannot be revived by access. That interval is currently fixed in code and has no configuration option.

Where an archived session lands depends on the storage backend: with a database backend it stays in the database with `status` set to `archived`; with file storage its session file is moved into the `sessions/archive/` subdirectory.

Both transitions are driven in bulk by `cleanup_expired`, which walks the session list, marks what is due for expiry, archives what is due for archival, and returns the number of sessions processed. A single session can also be archived directly through the API.

## History Management

### Message Limits

The `get_history(max_messages)` method returns recent message history, with a default limit of 500 messages.

### Consolidation Boundary Alignment

When truncating history, the system aligns to a safe boundary to prevent orphaned `tool-result` messages.

!!! warning "Consolidation Boundary"
    If the truncation point falls between a tool-call / tool-result message pair, the system
    automatically adjusts forward to avoid returning an orphaned tool-result without its
    corresponding tool-call.

### Adding Messages

```python
session.add_message(role="user", content="Hello")
session.add_message(role="assistant", content="Hello! How can I help you?")
```

## Configuration

Configure session behavior via `SessionConfig`:

```yaml
session:
  max_history_messages: 500      # Maximum messages in history
  expiry_hours: 72               # Session expiry time (hours)
  context_window_tokens: 128000  # Context window token limit
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_history_messages` | 500 | Maximum messages per history retrieval |
| `expiry_hours` | 72 | Inactivity period before session expires |
| `context_window_tokens` | — | Context window token limit |

## Multi-Channel Session Isolation

The Session Manager maintains independent message histories and model context for distinct session keys:

```yaml
# Same user with independent sessions across channels
- channel: slack
  user: alice
  chat: general
  thread: null

- channel: wechat
  user: alice
  chat: group-01
  thread: null
```

!!! info "Cross-Channel Search"
    The `session_search` tool supports searching across session histories from all channels,
    but session context itself remains isolated.

## WebSocket Sessions

`ws_session.py` provides session management for WebSocket long-lived connections:

- Maintains mapping between WebSocket connections and sessions
- Preserves session state after connection disconnects
- Supports session resumption when connections are restored

```yaml
gateway:
  session_policy:
    idle_timeout_minutes: 1440  # Default: 24 hours
```

## Session Search

The `session_search` tool enables searching across session histories:

- Full-text search across sessions
- Filter by channel, user, or time range
- Returns matching messages with surrounding context

## Dashboard Sessions Page

The Dashboard Sessions page provides visual session management:

- View all active sessions
- Inspect session details and message history
- Filter sessions by channel or user
- Monitor session status and expiry state

## Related Files

| File | Responsibility |
|------|----------------|
| `session/manager.py` | Session manager: isolation, persistence, expiry, archival |
| `gateway/session_context.py` | Gateway-layer session context |
| `gateway/session_policy.py` | Session policy configuration |
| `gateway/ws_session.py` | WebSocket session management |
