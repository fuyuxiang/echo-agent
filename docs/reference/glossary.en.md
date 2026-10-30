# Glossary

**Scope: both products; product-specific definitions are identified below.**

These definitions follow the current configuration, routes, and types in the source tree.

| Term | Definition |
|---|---|
| **A2A** | Agent-to-agent task protocol using JSON-RPC; the current implementation provides an inbound service. |
| **Agent Loop** | Processing loop from inbound event to response. |
| **Approval** | Review before a tool call, controlled by `permissions.approval.mode`. |
| **Channel** | Message adapter for endpoints such as Telegram, Discord, and CLI. |
| **Checkpoint** | Shadow Git snapshot of workspace files; excludes the database, sessions, memory, and logs, so it is not a full backup. |
| **Clarification** | Flow in which the agent asks the user for missing information. |
| **Compression** | Compaction of older session messages to control model-context length. |
| **Cron Job** | Job triggered by the scheduler. |
| **Dashboard** | Web interface using the Gateway API and `/ws/dashboard`. |
| **Execution Backend** | Backend selected by `tools.exec.host` for execution tools; `sandbox` copies the working directory but does not provide OS-level isolation. |
| **Gateway** | Service exposing HTTP APIs, WebSockets, and Dashboard assets. |
| **Knowledge** | Indexed documents for retrieval, stored separately from agent memory. |
| **Memory** | Persistent memory organized by type and scope, with retrieval and forgetting policies. |
| **Migration** | SQLite schema migration runs automatically during database initialization; `echo-agent migrate` only updates USER memory ownership and imports legacy `MEMORY.*.md` shards. |
| **Multi-Agent** | Task execution involving multiple collaborating agents. |
| **OpenTelemetry** | Optional trace and metric export framework; current code creates spans but not the formerly documented named business metric instruments. |
| **Pairing** | Gateway's short-lived code authorization for a platform/user identity; it does not issue an API token. |
| **Plugin** | Loadable Python extension that can register tools and lifecycle hooks; separate channel packages are not automatically wired. |
| **Risk Category** | Tool risk levels are `READ_ONLY`, `WRITE`, `EXEC`, and `DANGEROUS`. Names such as `MINIMAL_TOOLS` identify tool-profile sets, not risk levels. |
| **Security Profile** | Deployment profile: `personal_cli`, `daemon`, or `public_gateway`. |
| **Session** | Conversation context determined by channel, user, chat, and optional thread. |
| **Skill** | Knowledge or workflow described by `SKILL.md`; some candidate skills require approval. |
| **Spill** | Storage for oversized tool output, referenced in conversation and retrievable with `read_spill`. |
| **Tool** | Callable operation exposed to the agent; execution tools include `exec`, `execute_code`, and `process`. |
| **Tools Profile** | Tool admission set: `minimal`, `messaging`, `coding`, or `full`. |
| **Workspace** | Directory containing configuration, runtime data, and working files. |

## Product and workflow terms

| Term | Definition |
| --- | --- |
| Desktop / Client | Local desktop workspace with an embedded Rust execution engine. |
| Standalone Runtime | Python Agent service used through terminals, channels, or Gateway. |
| ACP | Agent Client Protocol; Desktop uses typed in-process channels for host-engine interaction. |
| Host | The component owning UI, resource management, local services, and engine lifecycle. |
| Artifact | An actual task output file whose contents and saved location can be verified. |
| Expert group | Desktop collaboration with persistent members, work, messages, and decisions. |
| Provenance | The origin and evidence supporting a fact, memory, or result. |
| Acceptance criteria | Observable conditions used to decide whether a task is complete. |
