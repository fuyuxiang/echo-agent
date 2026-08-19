# Discord

## Overview

The Discord channel connects via WebSocket Gateway (v10) for real-time event reception and uses the REST API to send messages. No public endpoint is needed; the bot initiates outbound connections to Discord's gateway.

Supports server (Guild) text channels and DM conversations. In servers, the bot can be configured to respond only when @mentioned.

!!! tip
    Gateway connections support automatic reconnection with Resume, allowing session recovery during network interruptions without re-fetching message history.

---

## Configuration Example

```yaml
channels:
  - type: discord
    token: "MTIzNDU2Nzg5.ABCdef.GHIjklMNOpqrsTUVwxyz0123456"
    allow_from:
      - "1234567890123456789"   # User ID (string)
      - "9876543210987654321"
    group_policy: mention               # open | mention
    reactions_enabled: true
```

| Field | Required | Description |
|-------|----------|-------------|
| `token` | Yes | Bot Token from the Discord Developer Portal |
| `allow_from` | No | Allowlist of user IDs; empty means unrestricted |
| `group_policy` | No | Server channel response strategy, defaults to `mention` |
| `reactions_enabled` | No | Enable reaction replies, defaults to `true` |

---

## Credential Setup

1. Visit the [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application** and enter an application name.
3. Navigate to the **Bot** section in the left sidebar, click **Add Bot**.
4. Click **Reset Token** to obtain the Bot Token.
5. On the Bot settings page, enable the following **Privileged Gateway Intents**:
   - `GUILDS`
   - `GUILD_MESSAGES`
   - `MESSAGE_CONTENT` (required to read message content)

!!! warning
    `MESSAGE_CONTENT` is a Privileged Intent. For bots in more than 100 servers, you must apply for verification with Discord. For small-scale use, simply enable it in the Portal.

### Inviting the Bot to a Server

Use the following URL format to invite the bot (replace `CLIENT_ID`):

```
https://discord.com/api/oauth2/authorize?client_id=CLIENT_ID&permissions=2147551232&scope=bot
```

The permission integer (`permissions=2147551232`) includes:

- Send Messages
- Send Messages in Threads
- Add Reactions
- Read Message History
- Manage Messages (for editing own messages)

!!! tip
    Inject the token via environment variable to avoid hardcoding:
    ```yaml
    token: "${DISCORD_BOT_TOKEN}"
    ```

---

## Callback/Webhook Setup

This channel uses a **WebSocket Gateway** connection. No HTTP Webhook configuration is needed.

On startup, the bot establishes a WebSocket connection to the Gateway and receives event pushes. The connection flow:

1. Send `IDENTIFY` with the Token and Intents bitmask.
2. Receive the `READY` event containing `session_id` and `resume_gateway_url`.
3. Send heartbeats at the interval specified by `heartbeat_interval`.
4. On disconnection, use `RESUME` to restore the session without message loss.

!!! question "Needs maintainer confirmation"
    Does the current implementation handle the Gateway's Invalid Session (op 9) event? This event requires a full re-IDENTIFY rather than RESUME.

---

## Capability Matrix

| Capability | Supported | Notes |
|-----------|-----------|-------|
| Edit messages | Yes | Via REST `PATCH /channels/{id}/messages/{id}` |
| Reactions | Yes | Via `PUT /channels/{id}/messages/{id}/reactions` |
| File sending | No | Not currently implemented |
| Real-time | Yes | WebSocket Gateway, very low latency |
| Group support | Yes | Server text channels |
| Message chunking | Yes | Auto-splits at 2000 characters |

---

## FAQ

### Bot is online but not receiving messages?

The most common cause is not enabling the `MESSAGE_CONTENT` Intent:

1. Go to Developer Portal → Application → Bot page.
2. Enable the **MESSAGE CONTENT INTENT** toggle.
3. Restart the bot.

If it still does not work, check whether `allow_from` is restricting the sender.

### Frequent disconnections and reconnects?

Check the following:

- Unstable network: Gateway requires a persistent WebSocket connection.
- Heartbeat timeout: Ensure heartbeats are sent at the `heartbeat_interval` returned by Discord.
- Rate limiting: Reconnecting too quickly triggers Discord's limits. The current implementation includes a 300-second backoff mechanism.

!!! warning
    If reconnection attempts are too frequent in a short period, Discord may temporarily ban the bot's Gateway access. The 300s backoff strategy is designed to prevent this.

### Message send returns 429 Too Many Requests?

Discord REST API has strict rate limits:

- Per channel: ~5 messages / 5 seconds
- Global: ~50 requests / second

The bot has built-in rate-limit response header parsing and automatic retry with wait. If this triggers persistently, you may need to reduce message send frequency.

### How to respond only in specific channels within a server?

Currently, filtering is done via `allow_from` by user ID. For channel-based filtering, channel IDs could be added to the configuration:

!!! question "Needs maintainer confirmation"
    Is filtering by channel ID supported? If not, this should be considered as a future feature.
