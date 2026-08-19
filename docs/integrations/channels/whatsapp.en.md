# WhatsApp

Connect Echo Agent via WhatsApp Business API.

---

## Overview

The WhatsApp channel uses Meta's WhatsApp Business API (Cloud API) and receives messages via Webhook.

## Configuration

```yaml
channels:
  whatsapp:
    enabled: true
    phoneNumberId: "your-phone-number-id"
    accessToken: "your-access-token"
    verifyToken: "your-verify-token"
    webhookPath: "/whatsapp"
    host: "0.0.0.0"
    port: 8085
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

1. Create an app at [Meta for Developers](https://developers.facebook.com/)
2. Add WhatsApp product
3. Get Phone Number ID and Access Token
4. Configure Webhook URL: `https://your-domain/whatsapp`
5. Set Verify Token to match your config
6. Subscribe to `messages` events
