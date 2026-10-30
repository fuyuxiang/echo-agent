# Gateway Authentication

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../../desktop/index.md).

Gateway provides three authentication modes to accommodate different scenarios from development to production deployment. The authentication system manages both user identity verification and API token authorization.

## Authentication Modes

### open Mode

Fully open with no user identity restrictions.

```yaml
gateway:
  auth:
    mode: "open"
```

!!! warning "Development Only"
    `open` skips platform/user allowlist and pairing checks. It does not disable independent API/admin token checks. Keep tokenless deployments on controlled loopback; exposed deployments require tokens, origin rules and tool policy.

### allowlist Mode

Only pre-configured users are allowed access. User identifiers support two formats:

- Plain user ID: `"user1"` — matches this ID on any platform
- Platform-qualified format: `"telegram:123456"` — matches only the specified user on the specified platform

```yaml
gateway:
  auth:
    mode: "allowlist"
    allowed_users:
      - "alice"
      - "bob"
      - "telegram:123456"
      - "wechat:wx_abcdef"
```

!!! tip "Platform-Qualified Format Priority"
    When the same user ID appears in both plain and platform-qualified formats, the platform-qualified format takes precedence. Using platform-qualified format is recommended in multi-platform environments to avoid cross-platform ID collisions.

### pairing Mode

Users complete initial authentication binding through temporary pairing codes with TTL (time-to-live) limits. Successfully paired user information is persisted. Allowlist is supported as a fallback mechanism.

```yaml
gateway:
  auth:
    mode: "pairing"
    allowed_users:
      - "admin_user"  # allowlist fallback — these users skip pairing
```

Pairing flow:

1. Admin generates a pairing code via API or dashboard (with limited validity)
2. New user enters the pairing code in their client
3. System validates the code and binds the user identity
4. Subsequent requests use the bound identity for automatic authentication

!!! warning "Pairing Code Security"
    Pairing codes should be transmitted to target users through secure channels (e.g., private messages, encrypted email). Codes are single-use and expire after being consumed.

## Token Authentication

Independent of user authentication, Gateway also provides token-based API access control:

### api_tokens (Standard Permissions)

Grants read and chat-level access, suitable for third-party integrations and automation scripts.

```yaml
gateway:
  auth:
    api_tokens:
      - "tk-proj-abc123def456"
      - "tk-integration-xyz789"
```

### admin_tokens (Admin Permissions)

Grants full admin-level access, implicitly including all permissions of standard tokens.

```yaml
gateway:
  auth:
    admin_tokens:
      - "atk-master-key-do-not-share"
```

!!! tip "Token Naming Convention"
    Consider adding meaningful prefixes (e.g., `tk-`, `atk-`) and purpose identifiers to tokens for easier audit trail tracking.

### Token Delivery

Tokens are passed via HTTP request headers. The default header name is `X-Echo-Agent-Token`:

```http
GET /api/v1/sessions HTTP/1.1
Host: localhost:58123
X-Echo-Agent-Token: tk-proj-abc123def456
```

The header name is configurable:

```yaml
gateway:
  auth:
    token_header: "X-Echo-Agent-Token"  # default
```

!!! warning "Admin operations accept headers only"
    HTTP management endpoints such as skill import/install/delete and knowledge
    upload/delete do not honour a `?token=` query parameter. An admin token must
    travel in a header, or it can end up in access logs, proxy logs, and referrers.

    The session WebSocket keeps `?token=` as a compatibility source for api tokens,
    but production clients should still prefer a header or the auth frame. See the
    [WebSocket protocol reference](../../reference/websocket-protocol.en.md).

## Loopback Exemption

Requests originating from `127.0.0.1` or `::1` (localhost) can bypass user identity authentication, simplifying local development and internal service-to-service calls.

!!! warning "Loopback Does Not Bypass Token Auth"
    The loopback exemption only skips user identity verification. For API endpoints requiring tokens (e.g., admin interfaces), local requests still need a valid `api_token` or `admin_token`.

!!! warning "DNS Rebinding Protection"
    The loopback exemption includes DNS rebinding protection via Host header validation. If the Host header is not in the `allowed_hosts` list, the request will be rejected even if the source IP is 127.0.0.1.

## Pairing Failure Lockout

`POST /api/v1/pair/verify` (the default path; `api_prefix` can change the prefix) accepts a pairing code from a client that does not yet have an API token. `platform`, `user_id`, and `code` must be non-empty strings of at most 128, 256, and 64 characters respectively. Invalid input returns `400` and is not counted as a failed attempt.

To slow repeated guesses for the same identity, the gateway uses this lockout policy:

- **Threshold**: 5 failures within a rolling 300-second window
- **Lockout duration**: Up to 300 seconds from the first failure in that window
- **Lockout granularity**: The request's `platform:user_id`. The client supplies `user_id`, so this is not a per-IP rate limit.

```
Attempts 1-4   → 403 (invalid or expired pairing code)
Attempt 5      → 429 with a Retry-After header and retry_after_seconds field
During lockout → 429 without checking the pairing code
After the window → Attempts are accepted again
```

Failure records are retained in memory by identity without a global entry limit or periodic reclamation of other identities. Public deployments should enforce network rate limiting or access controls in front of the gateway.

## Admin Users

In addition to `admin_tokens`, specific users can be granted admin permissions directly via the `admin_users` list:

```yaml
gateway:
  auth:
    admin_users:
      - "super_admin"
      - "telegram:999888"
```

`admin_users` is used for platform/user administrator checks. It does not replace the HTTP management endpoints’ admin-token guard; a claimed administrator user_id does not grant management API access.

## CORS Origin Whitelist

When Gateway is accessed by browser-based applications (e.g., web dashboards), configure the allowed CORS origins:

```yaml
gateway:
  auth:
    allowed_origins:
      - "https://my-dashboard.example.com"
      - "https://admin.example.com"
```

!!! tip "Development CORS"
    During development, add local addresses like `http://localhost:3000`. In production, strictly limit to actual domain names in use.

## Host Header Whitelist

Used to prevent DNS rebinding attacks and virtual host confusion:

```yaml
gateway:
  auth:
    allowed_hosts:
      - "gateway.example.com"
```

The request's Host header and the configured entries are normalised by the same function before comparison (`gateway/host_rules.py`): lowercased, port stripped, IPv6 folded to bracketed form. So `Echo.Example.com`, `echo.example.com:58123` and a bare `::1` all match — including values pasted straight out of a browser's address bar.

Wildcard addresses (`0.0.0.0`, `::`, an empty string) are not usable entries and are dropped. A browser sends the name from its address bar, never the wildcard the server bound to, so `allowed_hosts: [0.0.0.0]` matches nothing while looking configured — it counts as unconfigured and still warns at startup.

### What an empty `allowed_hosts` means

Resolution proceeds in three steps, and an empty list does **not** derive anything from the bind address:

| Case | Hosts accepted |
|------|----------------|
| `allowed_hosts` has a usable entry | Only the listed entries |
| Empty, bound to loopback | A fixed loopback set: `localhost`, `127.0.0.1`, `[::1]` |
| Empty, bound to a non-loopback address | No default — every Host is rejected |

The third case is deliberate: a deployment that binds to `0.0.0.0` must list its own domain explicitly, and the loopback exemption does not extend to attacker-supplied names. An empty Host header is likewise treated as untrusted.

"Bound to loopback" is decided by address, not by literal match: all of `127.0.0.0/8` (including `127.0.0.2`), `::1`, `[::1]` and `localhost` count as local. Leaving `host` empty does **not** — an empty string, like `::`, is a wildcard bind equivalent to `0.0.0.0`, and falls into the third case.

### What is actually affected

The third case does not make the whole dashboard unreachable. The Host check lives in `_check_csrf`, which only `_require_admin_token` calls, so:

- login, overview and other read-only pages (behind `_require_api_token`) keep working;
- native clients (`echo-agent cli`, curl — no Origin header) are unaffected;
- the admin endpoints — sessions, config, memory writes, tasks, cron, knowledge — all return 403 `cross-site request forbidden`.

The distinction matters: concluding "the warning is a false alarm" because the home page loads leaves every admin feature broken.

## Audit Logging

`gateway_auth/audit.jsonl` records authentication, management and pairing events at their instrumented boundaries. Fields are `ts` (Unix time), `action`, `platform`, `user_id`, `ok` and `reason`. Raw tokens are not included; this is not a claim of a separate record for every network request. Failed audit writes emit warnings, so absence of a failure record is not proof of success.

## Full Configuration Reference

```yaml
gateway:
  auth:
    mode: "allowlist"              # open | allowlist | pairing
    allowed_users:                 # allowlist / pairing fallback users
      - "alice"
      - "telegram:123456"
    admin_users:                   # admin user list
      - "super_admin"
    api_tokens:                    # standard API tokens
      - "tk-proj-abc123"
    admin_tokens:                  # admin tokens
      - "atk-master-key"
    token_header: "X-Echo-Agent-Token"   # token header name
    allowed_origins:               # CORS origin whitelist
      - "https://dashboard.example.com"
    allowed_hosts:                 # Host header whitelist
      - "gateway.example.com"
```

## Related Documentation

- [Gateway Overview](index.en.md)
- [Reverse Proxy Setup](reverse-proxy.en.md)
