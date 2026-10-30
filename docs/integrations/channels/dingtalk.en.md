# DingTalk

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../../desktop/index.md).

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
| File sending | ✅ | Local files/images in direct and group chats, up to 20MB |
| Real-time messages | ✅ | Stream Mode WebSocket push |
| Group chat | ✅ | Routed via `openConversationId` |
| Direct chat | ✅ | Routed via staffId |

**Send API distinction:**

- Direct messages: `POST /v1.0/robot/oToMessages/batchSend`
- Group messages: `POST /v1.0/robot/groupMessages/send` (requires `openConversationId`)

---

## Sending Files

Use the gateway's existing `send_file` tool:

```json
{
  "channel": "dingtalk",
  "chat_id": "cidYourGroupConversationId==",
  "file_path": "/absolute/path/report.docx",
  "caption": "Your report"
}
```

- Direct chats use the enterprise `userId` (`senderStaffId` in callbacks), not a personal DingTalk handle.
- Groups use the callback's `conversationId`, typically starting with `cid`, not the group number shown in the client. Without `conversation_type` metadata, tool events route `cid` IDs to the group API; explicit metadata takes precedence.
- Files: `xlsx`, `pdf`, `zip`, `rar`, `doc`, `docx`. Images: `jpg`, `jpeg`, `gif`, `png`, `bmp`, delivered using the image template. `as_image: false` keeps the file type; unsupported file formats require a ZIP archive. Only nonempty local files are accepted, up to 20MB each.
- Enable the enterprise robot message-sending permission and add the robot to the target group. Media-upload permission is enabled by default.

The channel validates files, uploads them through `POST https://oapi.dingtalk.com/media/upload`, then sends the returned `media_id` using `sampleFile` (`sampleImageMsg` for images). The caption is sent first as a separate text message. Upload errors, send errors, invalid recipients and recipient throttling return failure; a delivered caption alone never counts as a delivered file. A send receipt does not mean the recipient has read or downloaded the file.

API references: [media upload](https://open.dingtalk.com/document/orgapp/upload-media-files), [robot message types](https://open.dingtalk.com/document/orgapp/types-of-messages-sent-by-robots), [direct messages](https://open.dingtalk.com/document/orgapp/chatbots-send-one-on-one-chat-messages-in-batches), [group messages](https://open.dingtalk.com/document/orgapp/the-robot-sends-a-group-message).

---

## Long replies and delivery recovery

Long replies are split with numbered headers and encoded-size limits. SQLite stores each confirmed receipt, so retrying after a channel or process restart skips acknowledged parts. Captions and attachments also retain separate checkpoints. Slow inbound callbacks run behind a bounded queue while Stream pings remain responsive; callbacks are acknowledged only after admission.

Administrators can open **Channels → Delivery recovery** to inspect the full message, attachment names and confirmed part count. Definite rejections can be resumed. Timeouts, server errors and missing receipts require checking the target chat before choosing to retry or confirm that the entire message was received. Recovery preserves the original delivery identity and records the operator's decision. The APIs are `GET /api/v1/deliveries`, `GET /api/v1/deliveries/{id}` and `POST /api/v1/deliveries/{id}/recover`, using the configured gateway prefix.

Short text and final stream edits also retain recovery records. Checkpoints are bounded at 100,000 records and never automatically evict uncertain identities; new deliveries fail explicitly when capacity is exhausted. Lightweight adapters without SQLite retain checkpoints only for their current instance; DingTalk multipart text requires durable storage.

Attachment recovery requires the original file at its original path with unchanged contents. Restore that file before continuing, or send the changed file as a new message. Different content cannot reuse a previous delivery identity.

## Clarifying questions

A question becomes answerable only after confirmed delivery. The session remembers it across restarts, including its originating task. Replying with an option letter or free text continues the same task, plan and artifact workflow. Tools following a question in the same model batch wait for the answer. Option labels preserve legitimate text such as “A 股”.

Feishu and Telegram group replies from the question owner can continue without another mention. Other participants still follow normal group admission rules. Slack direct messages share a stable conversation; explicit threads remain separate. Pending questions follow `session.im_clarify_pending_ttl_seconds`; an explicit quote to another message takes precedence.

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

!!! question "Where does the group conversation ID come from?"
    No extra API call and no separate permission are involved. The conversation ID is read straight off the inbound callback's `conversationId` field and echoed back as `openConversationId` when replying.

    Direct messages (`conversationType` of `1`) use the sender ID as the conversation key; group chats use `conversationId`. So as long as the bot receives the message callback, it already holds the ID needed to reply.

**Q: What happens when rate limits are triggered?**

Text sends are spaced at least 3 seconds apart. Explicit platform rejections retain successful receipts and can be resumed from Delivery recovery. Unknown outcomes require checking the target chat before retrying. If rate limits are triggered frequently:

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
