# Slack

## Overview

Slack channel uses Socket Mode WebSocket (no public endpoint needed) with the Web API for messaging.

## Configuration

```yaml
channels:
  slack:
    enabled: true
    botToken: "xoxb-..."
    appToken: "xapp-..."
    allowFrom: []
    reactionsEnabled: true
```

## Credentials

1. Create a Slack App at [api.slack.com](https://api.slack.com/apps)
2. Enable Socket Mode → get App-Level Token (`xapp-`)
3. Install to workspace → get Bot Token (`xoxb-`)
4. Required scopes: `chat:write`, `reactions:write`, `app_mentions:read`, `im:history`, `im:read`

## Capabilities

| Capability | Supported |
|-----------|-----------|
| Edit messages | ✅ |
| Reactions | ✅ |
| File send | ❌ |
| Realtime | ✅ |
| Group chat | ✅ (threads) |

## Group Chat

Messages in channels use thread replies (`thread_ts`). The agent responds in the same thread.

## Common Issues

**Not receiving messages?**
- Ensure Socket Mode is enabled
- Check App Token starts with `xapp-`
- Verify event subscriptions include `message.im` and `app_mention`
