# Context Compression & Spill

When context approaches model window limits, Echo Agent applies compression and spill mechanisms.

---

## Three Mechanisms

### 1. Context Compression

As conversation history grows toward the model's context window, older messages are automatically compressed into summaries.

```yaml
compression:
  trigger_ratio: 0.7          # compress on reaching 70% of the window
  summary_target_ratio: 0.2   # token budget for the summary
  tail_budget_ratio: 0.4      # share of the window kept as recent messages
  head_protect_count: 3       # oldest messages never compressed
```

### 2. Tool Output Spill

When a tool produces output exceeding `max_inline_chars`, only a head/tail preview is sent to the model. The full output is stored as a spill artifact.

```yaml
spill:
  max_inline_chars: 6000  # characters before spill triggers
  retention_days: 7       # max retention
  max_total_mb: 512       # capacity limit
```

The model receives:
```
[Output spilled to file — showing first 2000 and last 2000 chars]
...
[Use read_spill tool to retrieve full content]
```

### 3. Spill Retrieval

The `read_spill` tool allows the model to retrieve specific portions:

- By character offset: `read_spill(path, offset=0, limit=5000)` — offsets are characters, not lines, so single-line output such as minified JSON stays reachable
- By regex pattern: `read_spill(path, pattern="ERROR.*")` — returns matching excerpts instead of a slice

## Security Boundaries

!!! warning "Important"
    - Spill files are **session-private** — each session can only access its own spills
    - Normal filesystem tools **cannot** read the spill directory
    - When `exec` is enabled, shell commands can still access spill files directly
    - Complete isolation only holds when execution tools are disabled
    - Retention is a maximum — capacity limits may trigger earlier cleanup
