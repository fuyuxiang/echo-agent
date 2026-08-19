# QQ Bot

## Overview

The QQ Bot channel connects via WebSocket gateway to QQ's official bot platform.

## Configuration

```yaml
channels:
  qqbot:
    enabled: true
    appId: "your-app-id"
    token: "your-token"
    secret: "your-secret"
    allowFrom: []
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
