# Context compression and output preservation

**Scope: standalone Runtime.** Model windows are finite while history and tool output grow. Compression reduces active history, Spill preserves oversized outputs, and consolidation retains reusable facts.

## Separate responsibilities

| Mechanism | Input | Goal | Retained form |
| --- | --- | --- | --- |
| Compression | Older active history | Make room for recent work | Summary and selected messages |
| Spill | Oversized tool output | Bound model input | Preview and retrievable original |
| Consolidation | Reusable information | Preserve facts and summaries | Scoped memory with provenance |

Compression does not establish committed memory. Spilled output is not a finalized deliverable.

## History compression

Near the effective context budget, older content is summarized while preserving necessary opening content, recent messages, and tool request/result alignment. Work happens on a copy; only a complete usable summary replaces active history.

Failed, truncated, or unavailable summaries retain the original content rather than saving an incomplete head-and-tail history. Preserving data does not guarantee an oversized request fits the model. Restore summarization, reduce input, or explicitly start a new session.

## Output preservation

When enabled, Spill stores large tool results in dedicated workspace storage and supplies a preview and reference. Later reads retrieve relevant ranges or pattern matches without resending everything.

References retain access-scope checks. Guessing another reference does not authorize reading it. Capacity and retention determine availability, so references are not permanent document URLs.

## Deliverables

When a user requests a document, create and verify a real artifact and provide the file or saved location. Logs, spilled output, and summaries support execution rather than replacing delivery.

See [Configuration](../reference/configuration.md), [Tasks and planning](../guides/tasks-planning.md), and [Memory](memory-system.md).
