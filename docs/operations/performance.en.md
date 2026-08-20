# Performance

Echo Agent's bottlenecks are usually model-call latency and local data I/O. This page covers the tuning levers that exist in the configuration.

---

## Where the time goes

Typical request breakdown:

```
┌─────────────────────────────────────────────────────────────┐
│ Model call (60-80%)  │ Tool execution (10-25%) │ I/O (5-15%) │
└─────────────────────────────────────────────────────────────┘
```

| Bottleneck | Symptom | Where to tune |
|------------|---------|---------------|
| Model latency | Long wait before the first token | Model selection, routing, compression |
| SQLite I/O | Slow memory retrieval | Storage path, periodic VACUUM |
| Process memory | OOM or swapping | Spill settings, session trimming |
| Knowledge retrieval | Slow similarity search | Chunking, index rebuild |
| Tool execution | External calls time out | Per-tool timeouts, parallel calls |

---

## Model calls

### Routing by task type

Route scenarios to appropriate models to balance cost against speed. Routing rules live in `models.routes` and match on `task_types`:

```yaml
models:
  default_model: claude-sonnet-4-5
  routes:
    - task_types: [simple]
      provider: openai
      model: gpt-4o-mini
      max_tokens: 1000
    - task_types: [coding]
      provider: anthropic
      model: claude-sonnet-4-5
      max_tokens: 8000
      fallback_models: [gpt-4o]
```

There is no `models.routing` section grouping models by scenario name — routing is always expressed as a `routes` list with `task_types`.

### Context compression

Echo Agent does not cache model responses (there is no `models.cache` in the configuration). The lever for reducing repeated input cost is context compression, under `compression`:

```yaml
compression:
  enabled: true
  trigger_ratio: 0.7          # compress when context usage hits this ratio
  summary_target_ratio: 0.2   # target share after compression
  tool_pruning_enabled: true  # prune historical tool output
```

### Parallel tool calls

Independent tools run in parallel. The setting is `agent.tool_concurrency`, enabled by default:

```yaml
agent:
  tool_concurrency:
    enabled: true
    max_concurrent: 4
```

!!! tip "Model fallback"
    Configure fallback models so a slow or failing primary model switches to a faster alternative. See [Model routing and fallback](../guides/models/routing-fallback.en.md).

---

## SQLite

Echo Agent stores metadata and memory in SQLite.

### WAL mode

SQLite connection parameters (`journal_mode`, `synchronous`, `cache_size`, `mmap_size` and other PRAGMAs) **cannot be tuned through configuration** — there is no `database` section, and the code sets none of these PRAGMAs.

What is configurable is the set of storage paths, under `storage`:

```yaml
storage:
  database_path: data/echo_agent.db
  sessions_dir: data/sessions
  memory_dir: data/memory
  logs_dir: data/logs
  spill_dir: data/spill
```

Database-level optimization therefore comes down to two things: keep `database_path` on a local SSD rather than a network filesystem, and run `VACUUM` periodically to reclaim fragmentation.

### Index maintenance

```bash
# Check database size and fragmentation
sqlite3 ~/.echo-agent/data/echo_agent.db "
  SELECT page_count * page_size as size_bytes FROM pragma_page_count(), pragma_page_size();
"

# Rebuild indexes (offline operation)
sqlite3 ~/.echo-agent/data/echo_agent.db "REINDEX;"

# Reclaim space
sqlite3 ~/.echo-agent/data/echo_agent.db "VACUUM;"
```

!!! tip "VACUUM regularly"
    SQLite does not shrink the file automatically after bulk deletes. Schedule VACUUM during low-load periods.

---

## Memory

### Context and history size

Controlled by `session` (`runtime` has a single field, `single_instance`, unrelated to memory):

```yaml
session:
  context_window_tokens: 0        # 0 means use the model's own window
  max_history_messages: 500       # history entries retained
  compression_window_cap: 200000  # upper bound on the compression window
  expiry_hours: 72                # session expiry
```

### Spill

Oversized tool output spills to disk. The section is top-level `spill`, not under `runtime`:

```yaml
spill:
  enabled: true
  max_inline_chars: 6000     # spill past this many characters
  max_total_mb: 512          # cap on total spill directory size
  retention_days: 7          # retention window
  sweep_interval_hours: 6    # cleanup scan interval
```

The spill directory path comes from `storage.spill_dir`. Artifacts are read back on demand with the `read_spill` tool — see the [tool reference](../reference/tools.en.md).

### Monitoring

```bash
# Process status and resource usage
echo-agent status

# Linux: detailed memory maps
cat /proc/$(pgrep -f echo-agent)/status | grep -i vm
```

---

## Knowledge retrieval

### Chunking and retrieval size

The knowledge index is a local JSON file, not an HNSW-style vector index, so there are no `ef_construction` / `ef_search` / `m` knobs and no `knowledge.index` subsection. What you can tune is chunking and retrieval size:

```yaml
knowledge:
  enabled: true
  chunk_size: 1200        # document chunk size (default 1200)
  chunk_overlap: 120      # chunk overlap (default 120)
  max_results: 5          # results per retrieval
  auto_index: true        # index automatically after changes
  index_path: data/knowledge_index.json
  docs_dir: data/knowledge
```

Chunks that are too small increase result count and recall noise; too large and you lose precision. Changes take effect only after an index rebuild.

### Index rebuild

The index is a derived artifact: when its structure is damaged, rebuild rather than repair. Use the `knowledge_index` tool (`action: rebuild`) or the Dashboard's Knowledge page.

Embedding and reranking parameters live in the `memory` section (`embedding_backend`, `rerank_enabled`, `rerank_top_k` and so on) — see the [configuration reference](../reference/configuration.en.md).

---

## Network

There is no unified `network` section, and no connection-pool or global-timeout fields. Network settings sit with their consumers:

| Setting | Purpose |
|---------|---------|
| `execution.network_policy` | Master outbound switch, defaults to `deny` |
| `tools.web.proxy` | Proxy for the web tool |
| `tools.web.timeout_seconds` | Web tool timeout, default `30` |
| `tools.browser.nav_timeout_sec` | Browser navigation timeout |
| `channels.telegram.proxy` | Proxy for the Telegram channel |
| `gateway.media_download_concurrency` | Gateway media download concurrency, default `4` |

```yaml
execution:
  network_policy: allow

tools:
  web:
    enabled: true
    proxy: "http://proxy:8080"
    timeout_seconds: 30
```

All outbound requests pass the shared SSRF check in `echo_agent/security/net_guard.py`; private addresses are rejected by default. Open the relevant `allow_private_addresses` only when intranet access is genuinely required.

---

## Resource guidance

### Minimum

| Resource | Minimum | Notes |
|----------|---------|-------|
| CPU | 2 cores | Needed for parallel tool execution |
| Memory | 2 GB | Base runtime plus SQLite cache |
| Disk | 1 GB | Database, logs, spill |
| Network | Stable connection | Model API calls |

### Recommended

| Resource | Recommended | Notes |
|----------|-------------|-------|
| CPU | 4 cores | Smooth tool parallelism |
| Memory | 4-8 GB | Comfortable cache headroom |
| Disk | 10 GB SSD | Fast I/O |
| Network | Low latency | Less time waiting on model calls |

---

## Diagnostics

```bash
# Runtime status and resource usage
echo-agent status

# Dependency status
echo-agent deps status

# Validate configuration (flags implausible values)
echo-agent config validate

# Cost analysis (find expensive operations)
echo-agent cost --group-by model
```
