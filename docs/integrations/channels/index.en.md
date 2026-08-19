# Message Channels

Echo Agent's channel system unifies message ingestion from diverse platforms into a single core processing pipeline. Each channel is implemented as an independent adapter inheriting from the `BaseChannel` base class, managed collectively by the `ChannelManager`.

## Core Architecture

### Three-Layer Structure

```
User Message → Channel Adapter → MessageBus → Agent Core
                                    ↑
                              ChannelManager
                           (register/start/route)
```

- **BaseChannel** — Abstract base class for all channels. Defines the unified interface: `connect()`, `disconnect()`, `send()`, `onMessage()`, and the capability declaration method `getCapabilities()`
- **MessageBus** — Message bus responsible for passing standardized message objects (`IncomingMessage` / `OutgoingMessage`) between channels and the Agent core
- **ChannelManager** — Channel manager that dynamically loads channel instances from configuration, handles start/stop, health checks, and failure recovery

### Message Standardization

All raw messages received by channels are converted into a unified `IncomingMessage` format:

```typescript
interface IncomingMessage {
  id: string;
  channel: string;        // channel identifier
  sender: string;         // sender ID
  content: string;        // text content
  attachments?: File[];   // attachment list
  replyTo?: string;       // quoted message ID
  metadata: Record<string, any>;  // channel-specific metadata
}
```

## Channel Capability Matrix

Each channel has different capabilities due to platform constraints. The Agent core uses this matrix to determine message delivery behavior (e.g., channels without edit support only receive final replies, not intermediate streaming updates).

| Channel | Connection | Msg Edit | Reactions | Files | Real-time | Group | Allowlist |
|---------|-----------|----------|-----------|-------|-----------|-------|-----------|
| Telegram | Long Polling / Webhook | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Discord | WebSocket (Gateway) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| WeChat Work | Webhook Callback | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| DingTalk | Webhook Callback | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Feishu (Lark) | Event Subscription | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Slack | Socket Mode / Events API | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Matrix | Client-Server API (Sync) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Email (IMAP/SMTP) | IMAP IDLE | ❌ | ❌ | ✅ | ⚠️ | ❌ | ✅ |
| Web Chat | WebSocket | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |
| CLI | stdin/stdout | ✅ (ANSI overwrite) | ❌ | ❌ | ✅ | ❌ | ❌ |
| Webhook (Inbound) | HTTP POST | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ |
| Webhook (Outbound) | HTTP POST callback | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ |
| SMS (Twilio) | Webhook Callback | ❌ | ❌ | ❌ | ⚠️ | ❌ | ✅ |
| Voice (Twilio) | WebSocket Stream | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ |

> ⚠️ = Delayed or platform-limited (Email depends on IDLE push intervals; SMS affected by carrier delivery latency)

## Channel Descriptions

### IM Channels

- **Telegram** — One of the most feature-complete channels. Supports both Long Polling and Webhook modes via Bot API, Markdown/HTML formatted messages, and Inline Keyboard interactions.
- **Discord** — Connects via Discord Bot Gateway. Supports Slash Commands, Thread conversations, and Embed rich text.
- **Slack** — Supports Socket Mode (no public IP needed) and Events API. Integrates Block Kit interactive components.
- **Matrix** — Open protocol with end-to-end encrypted room support. Ideal for self-hosted infrastructure scenarios.
- **WeChat Work** — Enterprise internal messaging. Receives messages via application callbacks, sends replies through proactive push API.
- **DingTalk** — Similar to WeChat Work. Supports both internal enterprise apps and group bot modes.
- **Feishu (Lark)** — ByteDance office platform. Supports event subscriptions, message cards, and rich text replies.

### General Channels

- **Web Chat** — Built-in web chat widget connecting directly to the Gateway via WebSocket. Suitable for embedding in product pages.
- **CLI** — Command-line interaction channel. Works with zero configuration. Streaming output uses ANSI escape sequences for character-by-character overwriting.
- **Email (IMAP/SMTP)** — Email channel using IMAP IDLE to monitor for new messages and SMTP to send replies. Supports attachments.

### Webhook Channels

- **Webhook (Inbound)** — Receives HTTP POST requests from external systems as message input. Suitable for CI/CD triggers, alert ingestion, etc.
- **Webhook (Outbound)** — Posts Agent results to a configured external URL after processing. Suitable for async notifications.

### Communication Channels

- **SMS (Twilio)** — Sends and receives SMS via Twilio API. Suitable for notifications, verification codes, and short messages.
- **Voice (Twilio)** — Enables voice conversations via Twilio Voice WebSocket with streaming STT/TTS processing.

## Common Configuration

All channels share a set of base configuration options:

```yaml
channels:
  <channel_name>:
    enabled: true                    # whether to enable
    allow_from:                      # sender allowlist (empty = allow all)
      - "user_id_1"
      - "user_id_2"
    group_policy: "mention_only"     # group chat policy: mention_only | all | disabled
    max_message_length: 4096         # max single message length
    timeout: 30000                   # request timeout (ms)
    retry:
      max_attempts: 3               # max retry attempts
      backoff: "exponential"        # retry strategy
```

### allow_from Allowlist

The allowlist controls which users can interact with the Agent:

- Empty or unconfigured — accepts messages from all users
- Configured with user ID list — only responds to listed users; other messages are silently discarded
- Supports wildcards — e.g., `"group:*"` allows all group chats, `"admin:*"` allows all administrators

### group_policy Group Chat Policy

Controls the Agent's trigger conditions in group chats:

| Policy | Behavior |
|--------|----------|
| `mention_only` | Only replies when @mentioned (default) |
| `all` | Responds to all messages in the group |
| `disabled` | Does not process group chat messages |

## Streaming & Progressive Delivery

Echo Agent's LLM responses are generated as a stream. For channels that support message editing, the system updates the sent message in real time, achieving a "typewriter effect":

```
Stream Tokens → Supports editing?
                ├─ Yes → Call editMessage() every N tokens to update content
                └─ No  → Wait for generation to complete, send final result once
```

### Edit Throttling

To avoid API rate limits, edit operations are throttled:

| Channel | Min Edit Interval | Notes |
|---------|-------------------|-------|
| Telegram | 1000ms | Bot API global limit 30 msg/s |
| Discord | 500ms | Rate limit per channel |
| Slack | 1000ms | Web API tier limits |
| Feishu | 500ms | Open platform rate limit |
| Web Chat | 100ms | Local WebSocket, no external limits |
| CLI | 50ms | Terminal refresh rate |

### Degradation Strategy

When a channel is temporarily unavailable (network interruption, API rate limiting), the system handles it with the following strategy:

1. **Retry** — Automatic retry according to configured retry policy
2. **Buffer** — Messages enter a local queue; sent in order once connection recovers
3. **Degradation Notice** — After exceeding the buffer threshold, the user is notified via an alternate channel
4. **Discard** — Messages past the maximum buffer time are discarded (configurable)
