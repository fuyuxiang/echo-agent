# Runtime Modes

Echo Agent offers three runtime modes for different scenarios from local development to production.

---

## Mode Comparison

| Mode | Command | Use Case | Persistence |
|------|---------|----------|-------------|
| Foreground | `echo-agent run` | Development, debugging | Process lifetime |
| Gateway | `echo-agent gateway` | Production, multi-channel | System service |
| CLI Client | `echo-agent cli` | Attach to running Gateway | N/A (thin client) |

## Foreground Mode

```bash
echo-agent run
```

Runs the agent in the current terminal. Suitable for:

- First-time setup and testing
- Development and debugging
- Single-channel usage (CLI channel only)

The process exits when the terminal closes.

## Gateway Mode

```bash
# Run in foreground
echo-agent gateway

# Install as system service
echo-agent gateway install
echo-agent gateway start
```

Gateway mode provides:

- HTTP/WebSocket API for external access
- Multi-channel support (all 14 channels)
- Dashboard web UI
- Background service management
- A2A protocol endpoint

## CLI Client Mode

```bash
echo-agent cli
```

Attaches to a running local Gateway as a thin TUI client. Requires Gateway to be running.

!!! tip
    Use `echo-agent status` to check if Gateway is running and which channels are active.
