---
name: email-assistant
description: "Read, search, draft, and send emails via Himalaya CLI or Python IMAP/SMTP. Requires email account configuration."
version: 1.0.0
metadata:
  echo:
    tags: [Email, IMAP, SMTP, Himalaya, Productivity]
    requires:
      anyBins: [himalaya, python3]
---

# Email Assistant

Two options: Himalaya CLI (modern, Rust-based) or Python imaplib fallback.

## Option A: Himalaya (Recommended)

```bash
brew install himalaya  # or cargo install himalaya
himalaya account configure
```

### Read

```bash
himalaya envelope list
himalaya envelope list --folder INBOX --page-size 10
himalaya message read <id>
```

### Search

```bash
himalaya envelope list --folder INBOX subject "weekly report"
himalaya envelope list from "boss@company.com"
```

### Send

```bash
himalaya message write
# Or via template:
himalaya template write | himalaya message send
```

### Reply/Forward

```bash
himalaya message reply <id>
himalaya message forward <id>
```

## Option B: Python Fallback

Environment variables:
```bash
export ECHO_EMAIL_HOST=imap.gmail.com
export ECHO_EMAIL_USER=user@gmail.com
export ECHO_EMAIL_PASS=app-password
export ECHO_SMTP_HOST=smtp.gmail.com
export ECHO_SMTP_PORT=587
```

```bash
python3 scripts/email_client.py list --host imap.example.com --user me@example.com --password xxx --count 10
python3 scripts/email_client.py search --host imap.example.com --user me@example.com --password xxx "urgent"
python3 scripts/email_client.py send --smtp-host smtp.example.com --user me@example.com --password xxx --to "a@b.com" --subject "Hi" --body "Content"
```

## Common Workflows

- Morning email digest: fetch unread, summarize subjects
- Draft reply: compose response based on context
- Search by sender/date/subject

## Security

- Store credentials in environment variables, never in chat
- Use app-specific passwords (Gmail requires this)
- Consider OAuth2 for production use
