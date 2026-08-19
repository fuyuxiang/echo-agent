# CLI Channel

The CLI channel provides terminal-based interaction with Echo Agent in foreground mode.

---

## Overview

The simplest interaction method. Automatically enabled in foreground mode (`echo-agent run`), no extra configuration needed.

## Configuration

```yaml
channels:
  cli:
    enabled: true
```

Enabled by default in foreground mode.

## Capabilities

| Capability | Supported |
|-----------|-----------|
| Edit messages | ❌ |
| Reactions | ❌ |
| File send | ❌ |
| Realtime | ✅ |
| Group chat | ❌ |

## Usage

```bash
# Foreground mode
echo-agent run

# Or connect to running Gateway
echo-agent cli
```

## TUI Commands

Local commands available in CLI:

- `/help` — Show help
- `/clear` — Clear screen
- `/copy` — Copy last reply
- `/details` — Show details
- `/save` — Save conversation
- `/theme` — Toggle theme
- `/quit` — Exit

Server commands (when connected to Gateway):

- `/approve` — Approve tool execution
- `/deny` — Deny tool execution
- `/approvals` — List pending approvals
- `/clarify` — Reply to clarification request
