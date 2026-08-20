# Memory Management Guide

Echo Agent's memory system uses a tiered architecture that mimics how human memory works — from short-term working memory to long-term archival storage. Each tier has different capacity, persistence, and retrieval characteristics.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                   Memory System                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Working Memory │───▶│    Eligibility Check       │    │
│  │ (max 20)      │    │    (eligibility.py)        │    │
│  └───────────────┘    └───────────┬───────────────┘    │
│                                   │                     │
│                                   ▼                     │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Episodic Memory│◀───│    Review & Quality        │    │
│  │ (temporal idx) │    │    (reviewer.py)           │    │
│  └───────┬───────┘    └───────────────────────────┘    │
│          │                                              │
│          ▼                                              │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Semantic Memory│◀──▶│  Contradiction Detection   │    │
│  │ (vector idx)   │    │  (contradiction.py)        │    │
│  └───────┬───────┘    └───────────────────────────┘    │
│          │                                              │
│          ▼                                              │
│  ┌───────────────┐    ┌───────────────────────────┐    │
│  │Archival Memory│◀───│    Consolidation           │    │
│  │ (long-term)    │    │    (consolidator.py)       │    │
│  └───────────────┘    └───────────────────────────┘    │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │           Retrieval Layer                        │   │
│  │   Vector │ BM25 │ Hybrid    (retrieval.py)      │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │    Local Embeddings & Reranking                  │   │
│  │    (local_embed.py / local_rerank.py)            │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

## The Four Memory Tiers

### Tier 1: Working Memory

Working memory is an in-process buffer that holds immediate context for the current conversation. Like human short-term memory, it has limited capacity and is not persisted.

| Property | Value |
|----------|-------|
| Max entries | 20 |
| Persisted | No |
| Context rendering | Markdown, max 2000 characters |
| Scope | Current conversation |

**Example:**

```python
# Working memory automatically collects key information from the conversation
working_memory.add(MemoryEntry(
    key="user_preference_lang",
    content="User prefers communicating in English",
    tier=MemoryTier.WORKING,
    type=MemoryType.USER
))

# Render context for model consumption
context = working_memory.get_context(max_chars=2000)
```

!!! note "Working memory does not persist across conversations"
    Working memory is lost when the conversation ends. If important information needs to be retained, the system uses Eligibility Check to determine whether to promote it to a higher tier.

### Tier 2: Episodic Memory

Episodic memory stores conversation episodes with temporal indexing. It records "what happened" — timestamped interaction records.

| Property | Value |
|----------|-------|
| Indexing | Temporal |
| Persisted | Yes |
| Content type | Conversation episodes, event records |
| Decay | Subject to time-based decay |

**Example:**

```python
# Episodic memory records user interaction history
episodic_entry = MemoryEntry(
    key="episode_2024_0315_debug_session",
    content="User requested help debugging a database connection issue; root cause was connection pool exhaustion",
    tier=MemoryTier.EPISODIC,
    type=MemoryType.USER,
    provenance=Provenance(source="conversation", timestamp="2024-03-15T10:30:00Z")
)
```

### Tier 3: Semantic Memory

Semantic memory stores structured knowledge with vector indexing for semantic retrieval. It records "what is known" — facts, preferences, rules.

| Property | Value |
|----------|-------|
| Indexing | Vector (local_embed.py) |
| Persisted | Yes |
| Content type | Structured knowledge, user preferences, environment info |
| Retrieval | Supports semantic similarity search |

**Example:**

```python
# Semantic memory stores structured knowledge
semantic_entry = MemoryEntry(
    key="user_tech_stack",
    content="User primarily uses Python + FastAPI for backend, Vue 3 for frontend",
    tier=MemoryTier.SEMANTIC,
    type=MemoryType.USER
)
```

### Tier 4: Archival Memory

Archival memory is the long-term storage tier, holding consolidated and verified important information. Analogous to human long-term memory.

| Property | Value |
|----------|-------|
| Storage | Persistent long-term |
| Source | Consolidated from upper tiers |
| Content type | Verified core knowledge |
| Decay | Minimal |

**Example:**

```python
# Archival memory holds consolidated core knowledge
archival_entry = MemoryEntry(
    key="project_architecture_v2",
    content="Project uses microservices architecture with 5 core services communicating via gRPC",
    tier=MemoryTier.ARCHIVAL,
    type=MemoryType.ENVIRONMENT
)
```

## Memory Lifecycle

A memory goes through a complete lifecycle from creation to eventual archival (or forgetting):

```
Creation ──▶ Eligibility ──▶ Review ──▶ Storage ──▶ Consolidation ──▶ Archival
  │                                       │              │
  │                                       ▼              ▼
  │                                     Decay ──▶ Forgetting
  │
  └──▶ Ineligible ──▶ Discarded
```

### Creation

Memories can be created in two ways:

1. **Implicit**: The system automatically extracts noteworthy information from conversations
2. **Explicit**: Manual storage via the `memory` tool

### Eligibility Check

`eligibility.py` determines what information is worth remembering:

- Explicit user preference statements
- Recurring patterns
- Environment configuration information
- Key project decisions

### Quality Review

`reviewer.py` performs quality control on memories that pass eligibility:

- Is the content clear and unambiguous?
- Does it duplicate existing memories?
- Does it include sufficient context?

### Consolidation

`consolidator.py` periodically merges and summarizes related memories:

```python
# Consolidation merges multiple related memories into a single refined entry
# Example: multiple mentions of "user prefers concise code style" get merged
# into one high-confidence memory
```

Consolidation triggers once the entry count reaches `memory.consolidationThreshold` (20 by default). `memory.sleepConsolidation` is on by default and runs an additional pass while the agent is idle.

### Decay and Forgetting

`forgetting.py` implements time-based memory decay:

- Memories that haven't been accessed for extended periods gradually lose priority
- Memories that decay below a threshold are marked as "forgotten"
- Forgetting does not mean deletion — the archival tier may still retain the information

Decay reduces the importance score over the period set by `memory.importanceDecayDays` (30 days by default). Memories scoring below `memory.archivalThreshold` (0.05) move to the archival tier; those below `memory.forgetThreshold` (0.01) are forgotten.

## Retrieval Modes

`retrieval.py` provides three retrieval modes suited to different scenarios:

### Vector Retrieval

Semantic similarity-based retrieval using embeddings generated by `local_embed.py`.

```python
# Semantic search — understands intent rather than matching keywords
results = memory.retrieve(
    query="user's programming language preferences",
    mode="vector"
)
```

**Best for:** Fuzzy queries, conceptually related information, cross-language matching.

### BM25 Retrieval

Traditional term frequency-inverse document frequency retrieval algorithm.

```python
# Exact keyword matching
results = memory.retrieve(
    query="FastAPI database connection",
    mode="bm25"
)
```

**Best for:** Exact term searches, code snippet matching, proper noun lookups.

### Hybrid Retrieval

Combines the strengths of Vector and BM25, with reranking via `local_rerank.py`.

```python
# Hybrid mode — balances semantic understanding with exact matching
results = memory.retrieve(
    query="database connection pool configuration",
    mode="hybrid"
)
```

**Best for:** Most general query scenarios (recommended as the default).

!!! tip "Choosing a retrieval mode"
    Use Hybrid mode for everyday use. Switch to Vector or BM25 only when you specifically need pure semantic matching or pure keyword matching.

## Prefetch Mechanism

`prefetch.py` implements proactive memory loading:

- At conversation start, predicts which memories will likely be needed based on context
- Pre-loads them into working memory to reduce retrieval latency
- Predictions are based on user historical behavior patterns

## Using the `memory` Tool

The `memory` tool provides explicit operations on the memory system:

### Store a Memory

```
memory store --key "project_db" --content "Project uses PostgreSQL 15" --type USER
```

### Search Memories

```
memory search --query "database configuration" --mode hybrid --limit 5
```

### Delete a Memory

```
memory delete --key "outdated_info"
```

### View Memory Status

```
memory status
```

!!! warning "Memory scope"
    The memory system is isolated per user (`memory_scope` provided by `ToolExecutionContext`). Memories are not visible across different users.

## Provenance Tracking

Every `MemoryEntry` includes provenance information, recording the origin and change history of the memory:

```python
class MemoryEntry:
    key: str           # Unique identifier
    content: str       # Memory content
    tier: MemoryTier   # Current tier
    type: MemoryType   # USER or ENVIRONMENT
    provenance: Provenance  # Provenance information
```

Provenance information includes:

- **Source**: Where the memory came from (conversation, tool call, consolidation)
- **Timestamp**: Creation and last modification time
- **Change chain**: History of consolidations and updates

This enables the system to:
- Trace any memory back to its original source
- Determine which memory is more trustworthy during contradiction detection
- Audit the complete lifecycle of a memory

## Contradiction Detection and Resolution

`contradiction.py` is responsible for discovering and handling conflicting memories:

### Detection Mechanism

When a new memory is written, the system automatically checks for contradictions with existing memories:

```python
# System detects a contradiction
# Existing memory: "Project uses MySQL database"
# New memory: "Project has migrated to PostgreSQL"
# → Contradiction detection triggered
```

### Resolution Strategies

1. **Recency priority**: More recent memories take precedence
2. **Provenance trust**: Explicit user statements > system inferences
3. **Confirmation mechanism**: When automatic resolution is not possible, flagged for confirmation

!!! note "Contradictions are never silently lost"
    When a contradiction is detected, the old memory is not deleted outright. Instead, it is marked as "superseded" and the full change history is preserved.

## Configuration Options

### Working Memory Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_entries` | 20 | Maximum number of working memory entries |
| `max_context_chars` | 2000 | Maximum characters rendered by `get_context()` |

### Retrieval Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `memory.retrievalOnMiss` | `degrade` | Behaviour on a retrieval-cache miss: `degrade` runs a time-bounded synchronous search and falls back to keyword search on timeout; `sync` always runs the full synchronous search |
| `memory.retrievalMissTimeoutSeconds` | `0.8` | Time budget for that bounded search; `0` skips it entirely |
| `memory.rerankEnabled` | `true` | Apply cross-encoder reranking to the fused top-K |
| `memory.rerankTopK` | `10` | How many fused candidates get reranked |
| `memory.rerankMinScore` | `0.0` | Absolute relevance floor; `0` reranks without dropping candidates |

### Decay configuration

| Option | Default | Description |
|--------|---------|-------------|
| `memory.importanceDecayDays` | `30.0` | Importance decay period, in days |
| `memory.archivalThreshold` | `0.05` | Memories scoring below this move to the archival tier |
| `memory.forgetThreshold` | `0.01` | Memories scoring below this are forgotten |

### Consolidation configuration

| Option | Default | Description |
|--------|---------|-------------|
| `memory.consolidationThreshold` | `20` | Entry count that triggers consolidation |
| `memory.sleepConsolidation` | `true` | Run an extra consolidation pass while idle |
| `memory.contradictionDetection` | `true` | Enable contradiction detection |

## Memory Types

The system supports two memory types:

- **USER**: User-related memories (preferences, habits, interaction history)
- **ENVIRONMENT**: Environment-related memories (project configuration, tech stack, system information)

## Best Practices

### 1. Let the System Work Automatically

In most cases, the memory system handles storage and retrieval automatically. Only use the explicit `memory` tool when you need to ensure specific critical information is remembered.

### 2. Use Meaningful Keys

```python
# Good keys
"user_preferred_language"
"project_deploy_target"

# Bad keys
"info1"
"temp"
```

### 3. Keep Content Concise and Clear

Each memory should be self-contained and independently understandable. Avoid overly long or ambiguous content.

### 4. Use Types to Differentiate Scope

- Personal user preferences → `USER` type
- Project/environment information → `ENVIRONMENT` type

### 5. Trust Contradiction Detection

When the system flags a memory contradiction, confirm which one is correct promptly. Do not ignore contradiction alerts.

### 6. Monitor Memory Status Regularly

Use `memory status` to understand the current distribution and health of your memories, ensuring important information hasn't been lost to decay.

!!! tip "About prefetch"
    The prefetch mechanism automatically loads relevant memories based on conversation context. If you find certain memories are consistently missing when needed, consider promoting them to the semantic memory tier to improve retrieval hit rates.
