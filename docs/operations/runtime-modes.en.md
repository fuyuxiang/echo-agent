# Runtime Modes

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Both `echo-agent run` and `echo-agent gateway` start the full Agent runtime. The former starts the HTTP/WebSocket Gateway only when `gateway.enabled` is true; the latter enables it for that run even when the setting is absent. Both run in the foreground by default. `echo-agent cli` is a thin client for an existing Gateway.

| Command | Gateway | Process management | Use |
|---|---|---|---|
| `echo-agent run` | Enabled when `gateway.enabled: true` | Current terminal | Local use and debugging |
| `echo-agent gateway` | Enabled | Current terminal | Foreground Gateway |
| `echo-agent gateway install`, `start` | Enabled | systemd or launchd | Background service |
| `echo-agent cli` | Connects to existing Gateway | Current terminal | Additional terminal clients |

Normally only one full Agent runtime should write a workspace. Do not run `run` and `gateway` against the same workspace at the same time.

## Local run

```bash
echo-agent run
```

The command selects the run mode; `gateway.enabled` determines whether `run` also exposes the Gateway. Set logging level through `observability.log_level` or `ECHO_AGENT_OBSERVABILITY__LOG_LEVEL`; `run` has no `--log-level` option. See [filesystem layout](../reference/filesystem-layout.en.md) for configuration lookup.

## Gateway service

```bash
echo-agent gateway                 # foreground
echo-agent gateway install         # register background service
echo-agent gateway start
echo-agent gateway status
echo-agent gateway logs --follow
```

The Gateway defaults to `127.0.0.1:58123` and an HTTP API prefix of `/api/v1`. It accepts WebSocket sessions and Dashboard connections. Management endpoints and message ingestion have different authorization checks. `gateway.auth.mode` (`open`, `allowlist`, or `pairing`) controls user authorization; `gateway.auth.api_tokens` controls API-token checks. An ordinary API token does not create full multi-tenant resource isolation. See [Gateway authentication](../integrations/gateway/authentication.en.md) and the [security model](../concepts/security-model.en.md).

See [background service](background-service.en.md) for install paths, stop timeout, and environment handling.

## CLI client

```bash
echo-agent cli
echo-agent cli --tui
echo-agent cli --port 58123 --token your-api-token
```

The CLI client connects to a local Gateway over WebSocket; it does not run another Agent. The inline terminal UI is the default; `--tui` selects the full-screen UI. Start the Gateway first. SSH port forwarding can expose a remote deployment through local loopback.
