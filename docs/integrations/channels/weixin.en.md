# WeChat Personal (iLink Bot)

## Overview

The WeChat Personal channel integrates with individual WeChat accounts via the iLink Bot API. It uses HTTP long-polling to receive messages, requiring no public endpoint — ideal for deployments behind NAT or firewalls.

!!! warning "Scope"
    This channel is for **personal WeChat accounts** only. It does NOT apply to WeChat Official Accounts or Mini Programs.

!!! tip "No Public IP Required"
    iLink Bot uses HTTP long-polling where the client pulls messages actively. No public port exposure or reverse proxy configuration is needed.

## Configuration Example

```yaml
channels:
  weixin:
    account_id: "wxid_xxxxxxxxxx"
    token: "your-ilink-bot-token"
    base_url: "https://ilinkai.weixin.qq.com"
    cdn_base_url: "https://cdn.ilinkai.weixin.qq.com"
    allow_from:
      - "friend_wxid_1"
      - "friend_wxid_2"
    dm_policy: "allow"          # allow | deny | allowlist
    data_dir: "./data/weixin"
    typing_indicator: true
```

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `account_id` | Yes | — | WeChat wxid |
| `token` | Yes | — | iLink Bot API Token |
| `base_url` | No | `https://ilinkai.weixin.qq.com` | API base URL |
| `cdn_base_url` | No | — | Media CDN URL |
| `allow_from` | No | `[]` (allow all) | Allowlist of wxids |
| `dm_policy` | No | `allow` | Direct message policy |
| `data_dir` | No | `./data/weixin` | Local data storage path |
| `typing_indicator` | No | `true` | Whether to send typing status |

## Credential Setup

### 1. Obtain iLink Bot Token

1. Visit the iLink Bot management console
2. Create a new Bot instance and obtain the API Token
3. Enter the Token in the `token` config field

### 2. QR Code Login Flow

```text
Start channel → Request login QR code → Display QR in terminal/logs → Scan with phone → Login success
```

!!! warning "Sessions expire and require re-scanning"
    How long a QR-login session stays valid is decided by the iLink Bot server, not by this project, so periodic re-scanning is required. An `errcode: -14` in the logs means the session has lapsed and must be re-established.

    This channel therefore cannot run fully unattended. For long-running deployments, alert on that error code so a re-scan can be done promptly.

## Callback/Webhook Setup

This channel does **not** require Webhook callback configuration. Messages are fetched via HTTP long-polling:

```text
Client ──(GET /messages)──→ iLink Bot API
Client ←──(JSON response)── iLink Bot API
```

Polling interval is controlled server-side; the client maintains a long connection waiting for new messages.

## Capability Matrix

| Capability | Supported | Notes |
|-----------|-----------|-------|
| Send text | Yes | Max 4000 characters |
| Send images | Yes | Via getuploadurl |
| Send voice | Yes | SILK encoded format |
| Send files | Yes | Via getuploadurl |
| Edit messages | No | WeChat does not support |
| Reactions | No | WeChat does not support |
| Group chat | No | Not implemented |
| Realtime messages | Yes | Long-polling |
| Typing indicator | Yes | With refresh loop |

## Technical Details

### Media Encryption

Media files are transmitted with AES-128-ECB encryption:

```text
Original file → AES-128-ECB encrypt → Upload to CDN
CDN download → AES-128-ECB decrypt → Original file
```

### Voice Messages

Voice messages use SILK audio encoding, the native audio format for WeChat. Audio must be converted to SILK format before sending.

### Typing Indicator Behavior

- WeChat typing bubble expiry: 5 seconds
- Refresh interval: 3 seconds (ensures bubble continuity)
- Maximum duration: 600 seconds
- Ticket TTL: 500 seconds

```text
[Start generating reply]
  ├── Send typing status
  ├── Wait 3s
  ├── Refresh typing status
  ├── ... (loop until reply ready or timeout)
  └── [Send reply message]
```

### Session Expiry Detection

When the API returns `errcode: -14`, the current session has expired and requires a new QR code login flow.

### Message Deduplication

Message IDs are tracked with a 300-second (5 minute) TTL to prevent duplicate processing during long-poll reconnections.

## FAQ

!!! question "Q: How long before I need to re-scan the QR code?"
    This depends on iLink Bot server-side session management. When `errcode: -14` appears in logs, re-scanning is required. Configure alerting on this error code.

!!! question "Q: What is the maximum message length?"
    Single text messages are limited to 4000 characters. Longer content must be split into segments.

!!! question "Q: How do I send images/files?"
    Use the `getuploadurl` endpoint to obtain an upload URL, upload the file to get a media ID, then reference that media ID in the send message call.

!!! question "Q: Why am I receiving duplicate messages?"
    Check that the deduplication mechanism is working. The default TTL is 300 seconds. If the service restarts and the dedup cache is lost, brief duplicates may occur.

!!! question "Q: Is group chat supported?"
    The current version does not support group message sending or receiving. Only direct (private) messages are processed.
