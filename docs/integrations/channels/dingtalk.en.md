# DingTalk

## Overview

The DingTalk channel communicates with the DingTalk Open Platform via **Stream Mode**. Stream Mode uses a callback registration + WebSocket long-polling mechanism, requiring no public-facing endpoint. This makes it ideal for deployments behind NAT, firewalls, or without a fixed public IP.

The channel supports both 1:1 (direct) and group chat message types, automatically distinguishing between them via `conversation_type` metadata and routing to the appropriate send API.

!!! tip
    Stream Mode is DingTalk's recommended bot integration approach, eliminating the complexity of webhook callback URL configuration and SSL certificate management.

---

## Configuration Example

```yaml
channels:
  dingtalk:
    enabled: true
    app_key: "your-app-key"
    app_secret: "your-app-secret"
    robot_code: "your-robot-code"
    allow_from:
      - "user1"
      - "user2"
```

| Field | Required | Description |
|-------|----------|-------------|
| `app_key` | Yes | AppKey of the Enterprise Internal App |
| `app_secret` | Yes | AppSecret of the Enterprise Internal App |
| `robot_code` | Yes | Unique robot identifier code |
| `allow_from` | No | Allowlist of user IDs permitted to interact; empty means no restriction |

---

## Credential Setup

1. Log in to the [DingTalk Open Platform](https://open-dev.dingtalk.com/)
2. Navigate to "App Development" → "Enterprise Internal Apps" → click "Create App"
3. On the app info page, obtain the **AppKey** and **AppSecret**
4. Go to the "Robot & Message Push" configuration page
5. Enable the robot feature and obtain the **robot_code**
6. In robot settings, enable **Stream Mode** (set message receiving mode to "Stream Mode")

!!! warning
    The AppSecret is displayed only once at creation time. Store it securely. If lost, you must regenerate it, which immediately invalidates the old key.

---

## Callback / Webhook Setup

Stream Mode requires **no public callback URL**. The connection flow is:

1. On startup, the app registers a callback with DingTalk API using AppKey + AppSecret
2. DingTalk returns a WebSocket connection endpoint
3. The app establishes a persistent WebSocket connection to receive message pushes
4. Automatic reconnection on disconnection

```
┌─────────┐   Register      ┌──────────────┐
│  Agent  │ ──────────────→  │  DingTalk API │
│         │ ←──────────────  │              │
│         │  WS endpoint     │              │
│         │ ═════════════════ │              │
│         │  WebSocket long connection       │
└─────────┘                  └──────────────┘
```

!!! tip
    Since it uses outbound WebSocket connections, services deployed behind NAT or firewalls work without issue. Only outbound HTTPS/WSS traffic (port 443) needs to be allowed.

---

## Capability Matrix

| Capability | Supported | Notes |
|-----------|-----------|-------|
| Message editing | ❌ | DingTalk API does not support modifying sent messages |
| Reactions | ❌ | Not supported |
| File sending | ❌ | Not currently implemented |
| Real-time messages | ✅ | Stream Mode WebSocket push |
| Group chat | ✅ | Routed via `openConversationId` |
| Direct chat | ✅ | Routed via staffId |

**Send API distinction:**

- Direct messages: `POST /v1.0/robot/oToMessages/batchSend`
- Group messages: `POST /v1.0/robot/groupMessages/send` (requires `openConversationId`)

---

## Authentication

DingTalk uses AppKey + AppSecret to obtain an `access_token`:

```
POST https://api.dingtalk.com/v1.0/oauth2/accessToken
{
  "appKey": "your-app-key",
  "appSecret": "your-app-secret"
}
```

The token is valid for 2 hours. The channel layer handles automatic refresh — no manual management required.

---

## FAQ

!!! question "Needs maintainer confirmation"
    Does obtaining `openConversationId` for group chats require additional event subscription permissions?

**Q: What happens when rate limits are triggered?**

DingTalk imposes rate limits on bot message sending. When throttled, the channel automatically backs off (300-second backoff). No manual intervention is needed. If rate limits are triggered frequently:

- Consolidate multiple short replies into a single message
- Check for abnormal duplicate message sends

**Q: Cannot connect via Stream Mode?**

1. Verify that AppKey / AppSecret / robot_code are all configured correctly
2. Confirm Stream Mode is enabled in robot settings
3. Check that outbound WSS connections are allowed (port 443)
4. Look for token acquisition failures in the logs

**Q: Bot doesn't respond in group chat?**

- Confirm `allow_from` is not excluding the sender
- Confirm the bot has been added to the target group
- Check if `conversation_type` is correctly identified as group chat
- Group chat may require @mentioning the bot to trigger a response (depends on configuration)

**Q: How are direct and group messages distinguished?**

The channel automatically distinguishes via the `conversationType` field in incoming messages:
- `1` = Direct message
- `2` = Group message

Different message types are automatically routed to the corresponding send API.
