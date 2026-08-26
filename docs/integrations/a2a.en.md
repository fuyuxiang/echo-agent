# A2A (Agent-to-Agent)

Echo Agent supports the A2A protocol for inter-agent task delegation.

---

## Overview

A2A enables agents to discover each other and delegate tasks via a standardized JSON-RPC protocol. Echo Agent can act as both A2A server (receiving tasks) and client (sending tasks to other agents).

## Agent Card

Served at `GET /.well-known/agent.json`:

```json
{
  "name": "echo-agent",
  "description": "A modular AI agent framework",
  "url": "http://localhost:58123",
  "version": "0.3.8",
  "capabilities": {"streaming": false, "pushNotifications": false},
  "skills": [{"id": "chat", "name": "chat"}, {"id": "tool_use", "name": "tool_use"}],
  "authentication": {"schemes": ["bearer"]}
}
```

## JSON-RPC Endpoint

`POST /a2a`

### Methods

| Method | Description |
|--------|-------------|
| `tasks/send` | Submit a task (synchronous) |
| `tasks/get` | Retrieve task status |
| `tasks/cancel` | Cancel a running task |

### Task States

```
submitted → working → completed | failed | canceled | input-required
```

## Configuration

A2A is enabled when Gateway is running. Configuration via `a2a` config section.

## Limitations

- No streaming (`tasks/sendSubscribe` not implemented)
- No push notifications
- Only text parts processed
- Session key format: `a2a:{task_id}`
