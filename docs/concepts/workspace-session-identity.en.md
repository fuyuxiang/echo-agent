# Workspace, session, and identity

Workspaces, sessions, memory scopes, and identities solve different problems. Separate conversations do not by themselves establish resource authorization.

| Concept | Purpose | Does not imply |
| --- | --- | --- |
| Workspace | Relevant files and local state | Complete OS-enforced filesystem isolation |
| Session | Related messages and execution context | Possession of an ID grants access |
| Memory scope | Durable facts visible to a task | All memory is shared within a session |
| Identity | Caller and authorized resources | One token isolates an entire tenant |

## Runtime routing and scope

Runtime generally identifies a session from its channel and conversation. Group context can be shared or split by sender. Memory scope is computed separately and may remain group-wide. Private-chat memory can follow bound identity.

Per-user group sessions therefore do not automatically imply per-user long-term memory. Check session policy, identity binding, and memory scope together.

## Continuity

Messages in a session are processed sequentially, with history and execution state persisted. Active history retains needed messages and keeps tool requests paired with results. Compression, consolidation, and history lookup support older content without sending everything on every turn.

Expiry, archiving, reset, and deletion are different operations. Failure to recall history does not prove erasure. See [Sessions](../guides/sessions.md) for retention and management.

## Desktop working directories

Desktop binds tasks to actual directories and coordinates authorization, IDE, and tools. Personal preferences can span projects; project facts are scoped to workspaces. Groups distinguish member context, member memory, and shared decisions.

Changing a directory or member model can establish a new context. History, memory, and real files are separate stores; clearing one does not clear the others. See [Tasks](../desktop/tasks-permissions.md) and [Expert groups](../desktop/collaboration-scheduling.md).

## Multiple users

Runtime supports multiple clients and sessions, but ordinary API tokens generally identify trusted callers of one instance. Mutually untrusted users or organizations need separate processes, data, workspaces, credentials, and network entry points. See [Tenant boundaries](security-model.md#multi-client-tenant-boundary).
