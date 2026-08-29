# A2A (Agent-to-Agent)

Echo Agent exposes an inbound A2A service so external agents can discover it and delegate text tasks to it.

---

## Overview

A2A uses Agent Cards and JSON-RPC to exchange tasks. The production runtime currently wires only the server side (receiving tasks). There is no CLI command, Agent tool, or peer configuration that delegates tasks outbound to another agent.

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

## Identity and task isolation

When Gateway has multiple API tokens, each token yields an opaque principal. Task storage, lookup, cancellation, in-flight run handles, and Agent sessions are all principal-scoped:

- Different tokens may use the same custom task ID without overwriting each other.
- A lookup or cancellation by token B for token A's task returns the same `Task not found` error as an ID that does not exist, so ownership is not disclosed.
- Authenticated requests use an internal `a2a:{opaque_hash}` session key containing neither the token nor its fingerprint. Single-principal, no-token deployments retain the compatible `a2a:{task_id}` form.

## Outbound client status

The package retains a low-level `A2AClient` Python helper, but it currently has no production caller and does not pass through the shared `net_guard` redirect-by-redirect SSRF validation and DNS pinning. It is therefore not a model-callable outbound delegation capability and must not be given model-generated or otherwise untrusted URLs.

## Limitations

- No streaming (`tasks/sendSubscribe` not implemented)
- No push notifications
- Only text parts processed
- No production outbound A2A delegation entry point
- Task retention is bounded by a TTL (3600 seconds by default) and count limit (1000 by default)
