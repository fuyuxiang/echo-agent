# Webhook Channel

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../../desktop/index.md).

The Webhook channel accepts inbound messages at its configured HTTP path. `channels.webhook` is disabled by default. When enabled, it defaults to `0.0.0.0:8080` and `/webhook`. This is separate from the Gateway's `/api/v1/message` endpoint.

## Configuration

```yaml
channels:
  webhook:
    enabled: true
    host: 127.0.0.1
    port: 8080
    path: /webhook
    secret: ""
    max_pending: 1000
```

An empty `secret` disables signature checks. Set a nonempty secret and a trusted network entry point before exposing the channel. The general configuration loader does not expand environment placeholders in YAML; override the field through the process environment:

```bash
export ECHO_AGENT_CHANNELS__WEBHOOK__SECRET="$WEBHOOK_SECRET"
```

## Request and signature

```bash
curl -X POST http://127.0.0.1:8080/webhook \
  -H 'Content-Type: application/json' \
  -d '{"sender_id":"ci-1","chat_id":"builds","text":"analyze the failed build","wait":false}'
```

`text` must be nonempty. Missing `sender_id` and `chat_id` each default to `webhook`. `content` is an alternative to `text`; `metadata` is optional. `wait` defaults to `false`.

With a nonempty `secret`, `X-Signature` must contain the **raw hexadecimal HMAC-SHA256 digest** of the exact request-body bytes, without a `sha256=` prefix. For example:

```python
import hashlib
import hmac

signature = hmac.new(secret.encode(), request_body_bytes, hashlib.sha256).hexdigest()
headers = {"X-Signature": signature}
```

A missing or invalid signature returns HTTP 403. Invalid JSON or fields return 400.

## Response

With `wait: false`, successful queue admission returns HTTP 200 and `{"status":"accepted","event_id":"..."}`. With `wait: true`, the connection waits for the final reply; success returns HTTP 200 and `{"response":"...","event_id":"..."}`. A 120-second wait timeout returns 504. Failed turns include `status`, `error`, `response`, and `event_id`, with an HTTP status determined by the turn result. This channel does not stream responses or send an asynchronous callback. To inspect an accepted event's turn state, query Gateway `GET /api/v1/turns/{event_id}` with management authorization.

`max_pending` limits only requests waiting synchronously for a reply; exceeding it returns 503. Inbound queue rejection also returns 503. Use an `Idempotency-Key` or `X-Idempotency-Key` header (or body `idempotency_key`) for safe retries; reusing a key with different content returns 409. See [idempotent retries](../../reference/gateway-api.en.md#idempotent-retries).
