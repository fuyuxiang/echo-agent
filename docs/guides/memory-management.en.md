# Memory Management

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Long-term memory is separate from conversation history. A Runtime using the same workspace can recall persisted facts across sessions. Clearing the terminal does not delete memory, and workspace-file checkpoints do not back it up.

## Tiers and ownership

| Tier | Owner and purpose | Lifetime |
|---|---|---|
| Working | In-process context for the current conversation epoch | Cleared on reset; not a long-term backup |
| Episodic | Conversation summaries in SQLite `memory_episodes` | Persisted episodes survive restart and support session filtering |
| Semantic | USER/ENVIRONMENT facts under `storage.memory_dir`, plus retrieval indexes | Written through MemoryService with persistence, invalidation and audit |
| Archival | Entries removed from active recall | Available to administration/maintenance, not ordinary low-weight Agent retrieval |

Superseded versions are also excluded from ordinary tool, retrieval and snapshot reads. Administrators can inspect version relationships without treating corrected facts as current. Source precedence prevents model inference from overwriting an explicit user statement; ENVIRONMENT writes also pass their own admission gate.

## Write, correct and delete

Ask the Agent to remember or correct a fact in an authenticated conversation. These are actual `memory` tool parameter objects, not CLI commands:

```json
{"action":"add","target":"user","key":"project_db","content":"The project uses PostgreSQL 15","source":"user_stated","tags":"project,database"}
```

```json
{"action":"search","target":"user","query":"project database"}
```

Inspect the original content before replacing it:

```json
{"action":"replace","target":"user","old_text":"The project uses PostgreSQL 15","content":"The project uses PostgreSQL 16","source":"user_stated"}
```

Inspect the entry before removing matching content:

```json
{"action":"remove","target":"user","old_text":"The project uses PostgreSQL 16"}
```

`list` lists visible facts. `list_contradictions` lists pending conflicts; `resolve_contradiction` takes `contradiction_id` and `winner_id`. See the [tool reference](../reference/tools.en.md#memory). There are no `memory store/delete/status` CLI commands or public `mode` parameter for selecting an internal retriever.

Writes pass scope, ENV admission, source-precedence and content checks. Failures report a reason. Successful removal invalidates retrieval and snapshot caches; a rejected removal is not a successful deletion.

## Identity and scope

Session keys own model context; `memory_scope` owns long-term facts. Owner-aware configuration can share memory across explicitly linked entrances for the same owner; group conversations retain their designed boundaries. Different tokens do not partition every instance resource into tenants. See [workspace, session and identity](../concepts/workspace-session-identity.en.md).

Dashboard Memory is an administrator's cross-subject view for browsing, filtering, searching and deleting. Ordinary API tokens remain restricted to their permitted scopes. Management endpoints include `GET {api_prefix}/memory`, `POST /memory/search`, `PUT /memory/{id}` and `DELETE /memory/{id}`; consult [Gateway API](../reference/gateway-api.en.md) for authorization and handler fields.

## Extraction, consolidation and forgetting

After a non-evaluation reply, ResponseStage checks unconsolidated message count and schedules a snapshot through the DURABLE background queue when the threshold is met. `sleep_consolidation` controls the Episode/fact/reflection stages of that work; it is not a separate idle-seconds timer. Only successful consolidation with a still-matching history advances `last_consolidated`; failure preserves the boundary for retry.

Extraction/review decides what is worth retaining. `eligibility.py` decides which entries a particular reading audience may see, using tier, provenance and task-state rules. Decay, reinforcement and conflict resolution follow their configuration and source rules; archival status does not promise permanent storage of essential facts.

`MemoryEntry` tracks source, creation/update times and version relationships. It has no `Provenance` child object for callers to construct. External content remains untrusted data after storage.

## Retrieval and dependency diagnosis

The retrieval pipeline combines keyword search, vectors and optional reranking. Working context, semantic facts and Episodes have different interfaces; do not assume universal entity/time filters. Episode `search_episodes` currently accepts query, session_key and limit.

Local embedding may download a model on first use. Inspect logs, dependencies and memory statistics when embeddings fail, and establish whether keyword fallback is active; fallback does not verify vector retrieval. See the [configuration reference](../reference/configuration.en.md) for memory, embedding, reranking and consolidation options.

## Restart, backup and verification

1. Persist a harmless test fact and confirm success before stopping the Runtime.
2. Restart with the same workspace/configuration, search from a new session, and check its scope and source.
3. Correct and search again; after deletion, confirm lists, retrieval and the next turn no longer use the old content.
4. Back up memory JSON, SQLite (including Episodes/version data), configuration and keys together. The vector index alone is insufficient. See [backup and restore](../operations/backup-restore.en.md).
