# Routing & Fallback Strategy

This document explains how Echo-Agent routes requests to specific models based on task type, and how degradation fallback is executed when a model becomes unavailable.

## Route Matching Logic

`ModelRouter` resolves the target model using the following priority:

1. **preferred_model** — Explicitly specified by the caller; highest priority
2. **task_type match** — Looks up a matching route in the routing table based on the request's `task_type`
3. **default_model** — Global default when none of the above match

On a successful match, `RouteDecision` contains:

| Field | Description |
|-------|-------------|
| `provider_name` | Provider identifier |
| `model` | Selected model ID |
| `fallback_chain` | Ordered degradation chain |
| `reason` | Why this route was selected |
| `context_window` | Context window size |
| `max_tokens` | Maximum output tokens |
| `temperature` | Sampling temperature |

### task_type values

`task_type` is inferred from the user's input by the framework and takes one of four values:

| Value | Triggered when |
|-------|----------------|
| `code` | The text contains code-related markers: `bug`, `class `, `def `, `typescript`, `python`, or their Chinese equivalents |
| `research` | The text contains retrieval intent: `search`, `find`, `look up`, or their Chinese equivalents |
| `planning` | The text contains planning intent: `plan`, `schedule`, or their Chinese equivalents |
| `chat` | Fallback when none of the above match |

Strings listed in `models.routes[].task_types` are matched against the inferred value case-insensitively. In addition, when a route declares no `task_types`, it matches if `task_type` equals that route's `provider` name or appears as a substring of its `model`.

## Health State Machine

Each Provider maintains an independent `ProviderHealth` instance. State transitions:

```
┌─────────┐  Failures hit threshold  ┌──────────┐  120s cooldown expires  ┌───────────┐
│ HEALTHY │ ────────────────────────→ │ COOLDOWN │ ──────────────────────→ │ HALF_OPEN │
└─────────┘                           └──────────┘                         └───────────┘
     ↑                                      ↑                               │       │
     │                                      │  Probe fails (within 2 tries) │       │
     │                                      └────────────────────────────────┘       │
     │         Probe succeeds                                                        │
     └───────────────────────────────────────────────────────────────────────────────┘
```

### State Descriptions

| State | Meaning |
|-------|---------|
| `HEALTHY` | Fully operational |
| `DEGRADED` | Reduced performance but still accepting requests |
| `COOLDOWN` | Rejecting all requests during cooldown (default 120 seconds) |
| `HALF_OPEN` | Allows up to 2 probe requests to verify recovery |
| `DISABLED` | Manually disabled; excluded from routing |

### ProviderHealth Tracked Fields

- `failure_count` — Consecutive failure counter
- `last_error` — Most recent error message
- `cooldown_until` — Timestamp when cooldown period ends
- `half_open_allowance` — Number of probe requests allowed in half-open state (max 2)

!!! note "Half-open probe mechanism"
    After entering `HALF_OPEN`, up to 2 requests are allowed through as probes:
    - Probe succeeds → state transitions to `HEALTHY`
    - Probe fails → immediately returns to `COOLDOWN` with a fresh timer

## Fallback Chain Resolution

When the primary model is unavailable (not `HEALTHY` or `HALF_OPEN`), `route_candidates()` builds the full degradation chain:

```
Primary model
  ↓ unavailable
Route-level fallback_models (tried in order)
  ↓ all unavailable
Global fallback_model
```

### Resolution Steps

1. Attempt `RouteDecision.model` (primary model)
2. Try each model in the route's `fallback_models` list in order
3. Fall back to the global `fallback_model`
4. At each step, check the target Provider's health status and skip unhealthy nodes

!!! warning "Complete fallback exhaustion"
    If all models in the fallback chain have Providers in an unavailable state, the request will return an error.
    It is recommended to always maintain at least one highly available global fallback_model.

## Context Window Resolution

The `context_window` is resolved via the `model_windows` configuration map:

```yaml
models:
  model_windows:
    "gpt-4o": 128000
    "claude-sonnet-4-20250514": 200000
    "gpt-4o-mini": 128000
    "deepseek-chat": 64000
```

During route resolution, `context_window` is looked up from the matched model name in `model_windows`. If no entry is configured for the model, a system-level default value is used.

## Configuration Example

```yaml
models:
  default_model: "gpt-4o"
  fallback_model: "gpt-4o-mini"

  routes:
    - model: "claude-sonnet-4-20250514"
      provider: "anthropic"
      task_types: ["code", "analysis"]
      fallback_models: ["gpt-4o", "deepseek-chat"]

    - model: "gpt-4o-mini"
      provider: "openai"
      task_types: ["chat", "summary"]
      fallback_models: ["gemini-2.0-flash"]

  model_windows:
    "gpt-4o": 128000
    "claude-sonnet-4-20250514": 200000
    "gpt-4o-mini": 128000
    "deepseek-chat": 64000
    "gemini-2.0-flash": 1000000
```

### Configuration Walkthrough

- Request with `task_type=code` → routed to `claude-sonnet-4-20250514`
- If Anthropic is unavailable → tries `gpt-4o`, then `deepseek-chat` in order
- If all are unavailable → uses global `fallback_model` (`gpt-4o-mini`)
- Requests not matching any route → uses `default_model` (`gpt-4o`) directly

!!! tip "Best Practices"
    - Configure multiple fallback_models for high-priority tasks
    - Choose a highly available, low-cost model as the global fallback_model
    - Define window sizes in model_windows for all models that may be used
