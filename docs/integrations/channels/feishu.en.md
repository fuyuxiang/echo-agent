# Feishu (Lark)

## Overview

The Feishu channel connects via Event Subscription webhook and REST API for sending messages.

## Configuration

```yaml
channels:
  feishu:
    enabled: true
    appId: "cli_xxx"
    appSecret: "your-app-secret"
    verificationToken: "your-verification-token"
    encryptionKey: "your-encryption-key"
    webhookPath: "/feishu"
    host: "0.0.0.0"
    port: 8083
    groupPolicy: mention  # mention | all
    botOpenId: "ou_xxx"
```

## Capabilities

| Capability | Supported |
|-----------|-----------|
| Edit messages | ❌ |
| Reactions | ❌ |
| File send | ❌ |
| Realtime | ✅ |
| Group chat | ✅ (mention policy) |

## Setup

1. Create app at [Feishu Open Platform](https://open.feishu.cn/)
2. Get App ID and App Secret
3. Configure Event Subscription URL: `https://your-domain:8083/feishu`
4. Get Verification Token and Encryption Key from event subscription settings
5. Subscribe to `im.message.receive_v1` event

## Authentication

Uses tenant_access_token (auto-refreshed from App ID + Secret).

## Common Issues

**Webhook verification fails?**
- Check `verificationToken` matches Feishu console
- Ensure `encryptionKey` is set if encryption is enabled
