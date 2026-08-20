# QQ Bot

## Overview

The QQ Bot channel connects via WebSocket gateway to QQ's official bot platform.

## Configuration

```yaml
channels:
  qqbot:
    enabled: true
    app_id: "your-app-id"
    app_secret: "your-app-secret"
    sandbox: false
    markdown_support: true
    media_enabled: true
    allow_from: []
```

## Capabilities

| Capability | Supported |
|-----------|-----------|
| Edit messages | ❌ |
| Reactions | ❌ |
| File send | ✅ |
| Realtime | ✅ |
| Group chat | ✅ |

## Setup

1. Register at [QQ Bot Platform](https://q.qq.com/)
2. Create an application
3. Get App ID, Token, and Secret
4. Enable relevant intents

## Common Issues

**Connection drops?**
- QQ Bot uses WebSocket with heartbeat; check network stability
- Verify credentials haven't expired
