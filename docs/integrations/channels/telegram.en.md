# Telegram

## Overview

The Telegram channel connects via Bot API Long-polling (`getUpdates`), requiring no public endpoint or Webhook server. This makes it ideal for deployments behind NAT or without a fixed IP address.

Supports both private chats and group conversations. In groups, the bot can be configured to respond only when @mentioned, reducing noise.

!!! tip
    In Long-polling mode, the bot actively pulls messages with typical latency < 1 second. For most use cases, the experience is indistinguishable from Webhook-based setups.

---

## Configuration Example

```yaml
channels:
  telegram:
    enabled: true
    token: "123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
    allow_from:
      - "100000001"        # quote numeric IDs: the field is list[str]
      - "100000002"
    proxy: "socks5://127.0.0.1:1080"   # Optional: socks5/http
    group_policy: mention               # open | mention
    reactions_enabled: true
    data_dir: "./data/telegram"         # Offset persistence directory
```

| Field | Required | Description |
|-------|----------|-------------|
| `token` | Yes | Bot Token obtained from @BotFather |
| `allow_from` | No | Allowlist of user/group IDs; empty means unrestricted |
| `proxy` | No | SOCKS5 or HTTP proxy address |
| `group_policy` | No | Group response strategy, defaults to `mention` |
| `reactions_enabled` | No | Enable reaction replies, defaults to `true` |
| `data_dir` | No | Offset file storage directory, defaults to `./data/telegram` |

---

## Credential Setup

1. Search for **@BotFather** in Telegram and start a conversation.
2. Send `/newbot` and follow the prompts to set a name and username.
3. Upon success, BotFather returns a token like `123456789:ABCdefGHI...`.
4. Place the token in the `token` configuration field.

!!! warning
    The token grants full access to the Bot. Never commit it to a public repository. Use environment variable injection instead:
    ```yaml
    token: "${TELEGRAM_BOT_TOKEN}"
    ```

To obtain user IDs for `allow_from`:

- Have the user send a message to the Bot and check the `from.id` field in logs.
- Or use third-party tools like @userinfobot.

---

## Callback/Webhook Setup

This channel uses **Long-polling** mode. No Webhook configuration is needed.

On startup, the bot automatically calls `deleteWebhook` to clear any existing Webhook settings, ensuring `getUpdates` works correctly.

!!! note "Switching from Webhook to polling"
    If the same bot token previously had a Webhook set by another framework, the first startup may take up to a minute before polling takes over — Telegram's Webhook revocation is not instant. During that window `getUpdates` may return nothing; wait it out rather than restarting repeatedly.

---

## Capability Matrix

| Capability | Supported | Notes |
|-----------|-----------|-------|
| Edit messages | Yes | Via `editMessageText` |
| Reactions | Yes | Uses `setMessageReaction` API |
| File sending | No | Not currently implemented |
| Real-time | Yes | Long-polling, latency < 1s |
| Group support | Yes | Groups and supergroups |
| Message chunking | Yes | Auto-splits at 4096 characters |

---

## FAQ

### Proxy settings not working?

Ensure the `proxy` field format is correct. Supported protocols:

- `socks5://host:port`
- `socks5://user:pass@host:port`
- `http://host:port`

Proxy support is implemented via `aiohttp_socks`. Make sure the dependency is installed.

### Bot not responding in groups?

1. Check the `group_policy` setting:
   - `mention`: Only responds when the message contains `@bot_username`.
   - `open`: Responds to all messages in the group.
2. Verify the Bot's **Group Privacy** setting: Send `/setprivacy` to @BotFather and select **Disable**. Otherwise the bot only receives `/commands` and @mentions.

### Offset file corruption causing duplicate messages?

Offset persistence uses atomic writes (write to temp file, then rename) to `data_dir`. Corruption should not occur under normal conditions. If it does:

1. Stop the Bot.
2. Delete the offset file in `data_dir`.
3. Restart the Bot. It will begin processing from the latest message (skipping history).

### HTML special characters causing send failures?

The bot uses HTML parse mode for messages. Characters `<`, `>`, and `&` are automatically escaped. If you manually construct HTML in templates, ensure user input is properly escaped.

!!! tip
    Messages exceeding 4096 characters are automatically chunked at paragraph boundaries. No manual handling is needed.
