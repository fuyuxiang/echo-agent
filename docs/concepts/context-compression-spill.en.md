# Context Compression & Spill

When context approaches model window limits, Echo Agent applies compression and spill mechanisms.

---

## Three Mechanisms

### 1. Context Compression

As conversation history grows toward the model's context window, older messages are automatically compressed into summaries.

```yaml
compression:
  triggerRatio: 0.7   # compress when reaching 70% of window
  targetRatio: 0.5    # compress down to 50%
```

### 2. Tool Output Spill

When a tool produces output exceeding the spill threshold, only a head/tail preview is sent to the model. The full output is stored as a spill file.

```yaml
spill:
  threshold: 8000     # characters before spill triggers
  retentionDays: 7    # max retention
  maxTotalMb: 500     # capacity limit
```

The model receives:
```
[Output spilled to file — showing first 2000 and last 2000 chars]
...
[Use read_spill tool to retrieve full content]
```

### 3. Spill Retrieval

The `read_spill` tool allows the model to retrieve specific portions:

- By character range: `read_spill(path, start=0, end=5000)`
- By regex pattern: `read_spill(path, pattern="ERROR.*")`

## Security Boundaries

!!! warning "Important"
    - Spill files are **session-private** — each session can only access its own spills
    - Normal filesystem tools **cannot** read the spill directory
    - When `exec` is enabled, shell commands can still access spill files directly
    - Complete isolation only holds when execution tools are disabled
    - Retention is a maximum — capacity limits may trigger earlier cleanup
