# Webhook (Generic)

## Overview

The Webhook channel provides a generic HTTP endpoint for custom integrations that don't fit other channel adapters.

## Configuration

```yaml
channels:
  webhook:
    enabled: true
    path: "/webhook"
    host: "0.0.0.0"
    port: 8080
    secret: ${WEBHOOK_SECRET}
```

## Capabilities

| Capability | Supported |
|-----------|-----------|
| Edit messages | ❌ |
| Reactions | ❌ |
| File send | ❌ |
| Realtime | ❌ |
| Group chat | ❌ |

## Usage

Send messages via HTTP POST:

```bash
curl -X POST http://localhost:8086/webhook \
  -H "Content-Type: application/json" \
  -d '{"message": "Hello", "user_id": "user1"}'
```

## Use Cases

- Custom chat interfaces
- IoT device integration
- CI/CD notifications
- Third-party service webhooks not covered by other channels
