# Email Channel

## Overview

The Email channel receives messages via IMAP polling and sends replies via SMTP. No public endpoint is required. It is suited for asynchronous communication scenarios such as support tickets and automated email replies.

Since email is inherently asynchronous, this channel is marked `is_realtime=False` — the agent delivers only the final generated message in one shot, with no streaming output.

## Configuration Example

```yaml
channels:
  email:
    enabled: true
    imap_host: imap.gmail.com
    imap_port: 993
    smtp_host: smtp.gmail.com
    smtp_port: 465
    username: bot@example.com
    password: ${EMAIL_APP_PASSWORD}
    use_ssl: true
    poll_interval_seconds: 30
    allow_from:
      - admin@example.com
      - support@example.com
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `imap_host` | string | — | IMAP server address |
| `imap_port` | int | 993 | IMAP port (SSL) |
| `smtp_host` | string | — | SMTP server address |
| `smtp_port` | int | 465 | SMTP port (SSL) |
| `username` | string | — | Login account |
| `password` | string | — | Password or app-specific password |
| `use_ssl` | bool | true | Enable SSL/TLS |
| `poll_interval_seconds` | int | 30 | IMAP polling interval in seconds |
| `allow_from` | list | [] | Sender allowlist; empty accepts all senders |

## Credentials

### Gmail

1. Go to [Google Account Security Settings](https://myaccount.google.com/security)
2. Enable 2-Step Verification (if not already enabled)
3. Generate a 16-character App Password under "App passwords"
4. Use this password in the `password` config field

!!! warning "Do not use your regular Gmail password"
    Google has disabled "Less secure app access" for direct IMAP/SMTP login with account passwords. You must use an App Password or the connection will be rejected.

### Outlook / Microsoft 365

1. Use `outlook.office365.com` (IMAP) and `smtp.office365.com` (SMTP, port 587)
2. If your organization enforces OAuth2, an admin must authorize IMAP access

### QQ Mail

1. Log in to QQ Mail → Settings → Account → POP3/IMAP/SMTP service
2. Enable IMAP/SMTP service and obtain the authorization code
3. Use the authorization code (not your QQ password) as `password`

!!! tip "QQ Mail configuration reference"
    - IMAP: `imap.qq.com:993`
    - SMTP: `smtp.qq.com:465`
    - The password field should contain the authorization code

## Capability Matrix

| Capability | Supported | Notes |
|-----------|-----------|-------|
| Edit sent messages | No | Email protocol does not support recall/edit |
| Reactions | No | — |
| File attachments | No | Current version does not process attachments |
| Realtime streaming | No | Async delivery, final result only |
| Group / multi-party | No | Separate sessions per sender |

## Internal Mechanics

### UID Watermark Persistence

The channel uses IMAP UIDs as watermarks to track processed emails. The watermark is atomically written to disk, ensuring:

- No duplicate processing of already-read emails after process restart
- No progress loss on abnormal exit (atomic write guarantees consistency)

### Email Processing Flow

1. IMAP poll fetches emails with UID > current watermark
2. HTML body is automatically converted to plain text (preserving readable structure)
3. Reply threading is tracked via Subject line (`Re:` prefix matching)
4. After the agent generates a reply, it is sent via SMTP with the `In-Reply-To` header set automatically

## FAQ

!!! question "What polling interval should I use?"
    30-60 seconds is recommended. Too short (<10s) may trigger rate limiting from email providers; too long increases user wait time. Gmail IMAP IDLE is not currently supported.

!!! question "What happens when allow_from is empty?"
    The channel will accept emails from all senders. In production, configuring an allowlist is recommended to prevent abuse.

!!! question "How are HTML emails handled?"
    Inbound HTML email bodies are automatically converted to plain text before being passed to the agent. Outbound replies are sent as plain text.

!!! question "Maintainer confirmation needed"
    Is attachment handling on the roadmap for future versions? The current implementation ignores attachment parts entirely.
