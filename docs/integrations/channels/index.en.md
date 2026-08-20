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

- **BaseChannel** — Abstract base class for all channels (`echo_agent/channels/base.py`). Three abstract methods must be implemented: `start()`, `stop()`, `send()`. Capabilities are declared as class attributes, not via a method call
- **MessageBus** — Message bus that passes standardized events between channels and the Agent core
- **ChannelManager** — Loads the channel instances listed in `_CHANNEL_REGISTRY` according to configuration and handles their lifecycle

### Capability Declaration

A channel that does not implement a capability simply leaves the corresponding class attribute at its default `False` — no method override needed. The Agent core uses these flags to decide how to deliver messages:

```python
class BaseChannel(ABC):
    supports_edit: bool = False                  # can edit an already-sent message
    supports_reactions: bool = False              # can add emoji reactions
    supports_files: bool = False                  # can send files
    supports_interactive_choices: bool = False    # supports interactive choices

    @abstractmethod
    async def start(self) -> None: ...

    @abstractmethod
    async def stop(self) -> None: ...

    @abstractmethod
    async def send(self, event: OutboundEvent) -> SendResult | None: ...
```

`send()` returns `SendResult | None` — a global handler receiving an event that is not its own must return `None`, otherwise it produces a bogus delivery receipt.

## Channel Capability Matrix

There are 14 channels, matching `_CHANNEL_REGISTRY` in `echo_agent/channels/manager.py` one for one. The capability columns below are taken from each adapter's class attribute declarations; anything not declared is `False`:

| Channel | Registry name | Connection | Msg Edit | Reactions | Send Files | Real-time |
|---------|---------------|-----------|----------|-----------|------------|:---------:|
| Telegram | `telegram` | Long Polling | ✅ | ✅ | ❌ | ✅ |
| Discord | `discord` | WebSocket (Gateway) | ✅ | ✅ | ❌ | ✅ |
| Slack | `slack` | Socket Mode (WebSocket) | ✅ | ✅ | ❌ | ✅ |
| Matrix | `matrix` | Client-Server API (long-polling sync) | ❌ | ✅ | ❌ | ✅ |
| WeChat | `weixin` | Long polling | ❌ | ❌ | ✅ | ✅ |
| QQ Bot | `qqbot` | WebSocket (Gateway) | ❌ | ❌ | ⚠️ | ✅ |
| WeCom (WeChat Work) | `wecom` | Webhook callback | ❌ | ❌ | ❌ | ✅ |
| DingTalk | `dingtalk` | WebSocket | ❌ | ❌ | ❌ | ✅ |
| Feishu / Lark | `feishu` | Webhook event subscription | ❌ | ❌ | ❌ | ✅ |
| WhatsApp | `whatsapp` | Webhook callback | ❌ | ❌ | ❌ | ✅ |
| Email | `email` | IMAP polling / SMTP send | ❌ | ❌ | ❌ | ❌ |
| Webhook | `webhook` | HTTP POST | ❌ | ❌ | ❌ | ❌ |
| CLI | `cli` | stdin/stdout | ❌ | ❌ | ❌ | ✅ |
| Cron | `cron` | Internal scheduler trigger | ❌ | ❌ | ❌ | ❌ |

The "Real-time" column reflects the `is_realtime` class attribute. `email`, `webhook` and `cron` are asynchronous channels; the rest are real-time. `supports_interactive_choices` is currently `False` on every channel.

> ⚠️ The QQ Bot's file capability is decided at runtime: `supports_files` is assigned from `config.media_enabled`, so it is `False` when media is off.

Always check `supports_files` before sending a file rather than assuming every channel can — today only `weixin` is unconditionally `True`, and `qqbot` depends on its configuration.

## Channel Descriptions

### International IM

- **Telegram** — The most capable channel, supporting message edits and reactions. Outgoing text has `<`, `>` and `&` HTML-escaped; set `metadata["telegram_markup"] = True` to opt out when you genuinely need to send raw markup.
- **Discord** — Connects via the Bot Gateway (WebSocket). Supports message edits and reactions.
- **Slack** — Connects via Socket Mode, so no public IP is required. Supports message edits and reactions.
- **Matrix** — Open protocol, received through the Client-Server API's long-polling sync. Supports reactions.
- **WhatsApp** — Receives messages via webhook callbacks.

### China IM

- **WeChat (`weixin`)** — Long-polling based, and currently the only channel that unconditionally supports sending files.
- **QQ Bot (`qqbot`)** — Connects via the Gateway (WebSocket); supports sending files once `media_enabled` is on.
- **WeCom (`wecom`)** — Enterprise internal app receiving messages via callbacks; payload encryption lives in `wecom_crypto.py`.
- **DingTalk (`dingtalk`)** — Connects over a WebSocket long connection, so no public callback URL is needed.
- **Feishu / Lark (`feishu`)** — Connects via webhook event subscriptions.

### Non-IM Channels

- **CLI (`cli`)** — Command-line interaction channel, usable with zero configuration.
- **Email (`email`)** — Receives via IMAP polling and sends via SMTP.
- **Webhook (`webhook`)** — Integrates external systems over HTTP POST. Suitable for CI/CD triggers, alert ingestion, and similar cases.
- **Cron (`cron`)** — Fired by the internal scheduler on a schedule. It has no external counterpart and is used for recurring proactive tasks.

## Common Configuration

Most channels share the following base options (for the full per-field reference see the [configuration reference](../../reference/configuration.md), which is generated from the schema):

```yaml
channels:
  telegram:
    enabled: true                    # enable this channel
    token: "..."                     # platform credential; the field name varies by channel
    allow_from:                      # sender allowlist (empty = no restriction)
      - "user_id_1"
    group_policy: "mention"          # group chat policy: open | mention
```

For channel-specific fields (such as Telegram's `proxy`, `data_dir` and `reactions_enabled`), see that channel's own page.

### allow_from Allowlist

Controls which users can interact with the Agent; the check lives in `BaseChannel.is_allowed()`:

- Empty or unconfigured — accepts messages from all users
- Configured with a user ID list — only responds to listed users; everything else is silently discarded

### group_policy Group Chat Policy

Controls when the Agent replies in group chats. There are only two values:

| Policy | Behavior |
|--------|----------|
| `mention` | Only replies when @-mentioned (default) |
| `open` | Responds to all messages in the group |

To avoid handling group chats at all, restrict senders with the allowlist or disable the channel.

## Streaming & Progressive Delivery

LLM responses are generated as a stream. Whether streaming incremental replies are enabled for a channel is decided by `stream_channels`, which defaults to `cli`, `telegram`, `discord`, `slack` and `gateway:*` (a `prefix:*` wildcard is supported). Once enabled, channels that support editing keep updating the message already sent, producing a "typewriter effect"; channels without edit support wait for generation to finish and send once.

### Flush Throttling

Every incremental update costs an edit API call on IM channels, so flushes are governed by both a character threshold and a time interval — whichever is hit first triggers a flush:

| Option | Default | Effect |
|--------|---------|--------|
| `stream_flush_chars` | 180 | Flush once this many characters have accumulated |
| `stream_flush_interval_ms` | 1500 | Maximum interval between flushes (ms) |
| `stream_paragraph_mode` | `true` | Prefer splitting on paragraph boundaries |

Local channels (`cli`, `gateway` websocket) face no rate limits and frames are cheap, so they get their own low-latency tier:

| Option | Default | Effect |
|--------|---------|--------|
| `stream_local_flush_chars` | 24 | Character threshold for local channels; `0` reuses the shared value |
| `stream_local_flush_interval_ms` | 100 | Maximum flush interval for local channels (ms) |

Raising the two shared values reduces edit calls and the chance of tripping platform rate limits, at the cost of a choppier-looking reply.
