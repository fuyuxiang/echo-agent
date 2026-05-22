# Echo Agent

<p align="center">
  <strong>🧬 Self-Evolving Skill Library × 🧠 Cognition-Grade Memory System</strong>
</p>

<p align="center">
  <em>A self-hosted, long-running agent system — skills that evolve from runtime trajectories, memory that persists beyond the session window.</em>
</p>

<p align="center">
  <a href="README.md">中文</a> ·
  <a href="README.en.md">English</a>
</p>

<p align="center">
  <a href="#core-capabilities">Core Capabilities</a> ·
  <a href="#project-status">Status</a> ·
  <a href="#quickstart">Quickstart</a> ·
  <a href="#self-evolution">Self-Evolution</a> ·
  <a href="#cognitive-memory-system">Cognitive Memory</a> ·
  <a href="#architecture">Architecture</a>
</p>

<p align="center">
  <img alt="Python 3.11+" src="https://img.shields.io/badge/python-3.11%2B-3776AB.svg?logo=python&logoColor=white&style=for-the-badge">
  <img alt="Self Evolving" src="https://img.shields.io/badge/self--evolving-✓-22c55e?style=for-the-badge">
  <img alt="Status Alpha" src="https://img.shields.io/badge/status-alpha-f59e0b.svg?style=for-the-badge">
  <a href="LICENSE"><img alt="License MIT" src="https://img.shields.io/badge/license-MIT-blue.svg?style=for-the-badge"></a>
  <img alt="Self Hosted" src="https://img.shields.io/badge/self--hosted-111827?style=for-the-badge">
</p>

---

## Overview

**Echo Agent** is a long-running agent system for private infrastructure, introducing a complete runtime improvement loop on top of traditional agent frameworks.

Each task execution is recorded as a structured Trajectory. The Evolver module consumes these trajectories and uses an LLM to propose candidate skill changes. Candidates are not applied directly; they first undergo a baseline-vs-candidate A/B comparison on an evaluation dataset, and are promoted into the skill library only when metrics strictly outperform the baseline. Failed candidates are automatically rejected, and any promotion can be reverted via a single rollback command. The full loop — record, reflect, propose, evaluate, promote, cooldown — executes on the operator's own servers, with no telemetry sent to external services beyond model API calls.

Beyond the evolution core, Echo Agent provides multi-role collaboration (planner / coder / researcher / operator), a four-tier memory system (Working / Episodic / Semantic / Archival), LLM-driven approval for high-risk tools, native A2A and MCP protocol support, and a unified messaging layer covering 12+ channels including Telegram, Discord, Slack, WeChat, QQ, and Feishu.

Supports OpenAI, Anthropic Claude, Google Gemini, AWS Bedrock, OpenRouter, and any OpenAI-compatible endpoint.

---

## Core Capabilities

The next inflection point for agent frameworks lies neither in tool count, nor in model adapters, nor in orchestration syntax — it lies in two long-overlooked questions: **does capability grow with runtime**, and **does memory persist beyond the session boundary**. Echo Agent rebuilds its primitives around exactly these two.

<table>
<tr>
<td width="50%" valign="top">

### 🧬 Self-Evolving Skill Library

**Skills evolve with runtime, instead of being frozen at deployment.**

Traditional agent frameworks lock the capability boundary at deployment time; failure modes observed at runtime cannot flow back into skill definitions. Echo Agent turns each task execution into a verifiable improvement signal, closing the loop from runtime data back into the skills themselves.

- **Trajectory capture.** Every task, tool invocation, and reflection score within the Agent Loop is recorded as a structured Trajectory and persisted to SQLite.
- **Candidate generation.** The Evolver consumes failing and low-scoring trajectories; an LLM submits candidate changes (`create` / `patch` / `disable`) via structured tool calls, each annotated with a falsifiable expected-improvement metric.
- **A/B-evaluated promotion.** `PromotionGate` snapshots the skill directory, applies the candidate to an isolated copy, and compares baseline against candidate on the evaluation dataset; only candidates strictly outperforming baseline are promoted.
- **Regression gate and cooldown.** Candidates regressing beyond `regression_threshold` are rejected outright; promoted skills enter a 24-hour cooldown by default; `evolution rollback <skill>` reverts the most recent promotion.
- **Full audit trail.** Trajectories, candidate skills, and evolution runs are persisted; every candidate is traceable to its source trajectories.

```bash
echo-agent evolution status         # engine state and pending candidates
echo-agent evolution run            # trigger a full evolution pass manually
echo-agent evolution rollback <id>  # revert the most recent promotion
```

→ [Full design and configuration](#self-evolution)

</td>
<td width="50%" valign="top">

### 🧠 Cognition-Grade Memory System

**Memory transcends the context window and persists across sessions.**

A single-tier vector store cannot model temporal decay or semantic conflict; hard-truncating the context window does not constitute long-term memory. Drawing on the tiered model from cognitive science, Echo Agent provides full lifecycle management from working memory to archival storage.

- **Four-tier hierarchy.** Working / Episodic / Semantic / Archival, covering the full lifecycle from in-process buffer to cold archive.
- **Ebbinghaus adaptive decay.** `half_life = base × (1 + log₂(1 + access_count))`; access frequency governs half-life, with effective importance below threshold triggering automatic demotion or deletion.
- **Contradiction detection over a versioned memory lattice.** Semantic conflicts between new and existing memories are not silently overwritten; they are recorded as temporal edges in the graph, enabling belief revision and historical traversal.
- **Hybrid retrieval (Resonance Scoring).** BM25 and FAISS vector similarity are combined under query-entropy adaptive weighting; the Ebbinghaus decay factor participates in the rerank stage.
- **Sleep-time consolidation pipeline.** After a session ends, `MemoryConsolidator` and `MemoryReviewer` perform episode generation, semantic-fact extraction, contradiction detection, and archival sweep.

> **Conflict example.** A user previously asserts an aversion to dense visual patterns and later expresses a preference for densely starred night skies. The system identifies the potential belief conflict, writes a temporal edge instead of overwriting the prior memory, and exposes the full belief-change chain to the agent during context construction in subsequent related tasks.

→ [Full design and retrieval mechanics](#cognitive-memory-system)

</td>
</tr>
</table>

### Coupling

```text
Cognitive Memory ──→ supplies high-quality trajectories & context ──→ Self-Evolution proposes better candidates
        ▲                                                                    │
        │                                                                    │
        └──────────── better skills produce better trajectories ─────────────┘
```

The memory system supplies learnable samples and contextual signal to the evolution engine; the evolution engine in turn improves trajectory quality and skill-library utility. The two form the system's primary feedback loop — and the fundamental distinction between Echo Agent and one-shot orchestration frameworks.

---

## Why Echo Agent

Beyond the core capabilities above, Echo Agent provides:

- **Fully self-hosted.** Aside from model API calls, trajectories, memories, and conversations never leave the local environment; persistent data resides in local SQLite and the filesystem.
- **Unified message bus.** CLI, webhooks, scheduled jobs, 12+ messaging channels (Telegram / WeChat / Feishu / Slack, etc.), and the Gateway API share the same Agent Loop, with consistent session, memory, tool, and permission boundaries across all entry points.
- **Fine-grained permission model.** High-risk tools (shell, file write, code execution) enter the approval flow by default, governed by LLM risk assessment, path policy, and human admin review.
- **Multi-agent collaboration.** Specialized roles (planner, coder, researcher, operator) auto-route by task, with parallel execution and long-task orchestration.
- **Native A2A + MCP.** Implements Google's A2A protocol (discoverable and callable by external agents); integrates Anthropic's MCP (tools from any MCP server can be mounted).

---

## Project Status

Echo Agent is in **alpha**. Configuration fields, internal storage schemas, and APIs may change before a stable release.

| Area | Status | Notes |
|------|--------|-------|
| CLI runtime | Beta | Interactive and foreground execution are supported |
| Configuration & credentials | Beta | Includes interactive setup wizard and multi-provider routing |
| Gateway (REST / WebSocket) | Alpha | Authentication required for public deployments |
| Self-Evolution | Experimental | Use `auto_promote: false` for production-like environments |
| Four-tier memory system | Experimental | FAISS is optional; falls back to keyword retrieval when missing |
| A2A / MCP protocols | Experimental | The protocols themselves are still evolving |
| Channel adapters | Experimental | Stability depends on third-party APIs and adapter quality |
| Evaluation framework | Experimental | Evolution quality depends on dataset coverage |

---

## Quickstart

### Requirements

- Python **3.11+**
- Linux, macOS, or WSL2
- An API key for at least one model provider (OpenAI / Anthropic / Gemini / Bedrock / OpenRouter / any OpenAI-compatible endpoint)
- [`uv`](https://docs.astral.sh/uv/) is recommended for environment management

### Install from source (recommended)

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

uv venv venv --python 3.11
source venv/bin/activate
uv pip install -e ".[all]"

echo-agent setup -w .   # interactive configuration wizard
echo-agent run -w .     # run in foreground
```

### Install script (development environments only)

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh         # please review before executing
bash install.sh
```

> The install script modifies `PATH` and (on Linux) registers a systemd service. For production deployments, prefer the source install path and manage virtualenv / service registration yourself.

### 3-minute hello world

```bash
# 1. Launch the interactive CLI
echo-agent

# 2. Try any task in the prompt
> write a Python script that monitors free disk space, save it as disk_check.py

# 3. Observe:
#   - Agent plans → tool calls → high-risk tool (write_file) goes through approval
#   - Once the task ends, this trajectory is persisted to SQLite for later evolution

# 4. Check evolution status
echo-agent evolution status
```

---

## Self-Evolution

> Traditional agent frameworks lock their capability boundary at deploy time. Echo Agent turns each task execution into a verifiable improvement signal so the skill library can keep evolving as the system runs.

### The pipeline

```text
   ┌──────────────────────┐
   │  TrajectoryRecorder  │  capture full trajectory of every Agent Loop run
   └──────────┬───────────┘
              │
   ┌──────────▼───────────┐
   │       Evolver        │  LLM proposes candidate skill changes from trajectories
   └──────────┬───────────┘
              │
   ┌──────────▼───────────┐
   │    PromotionGate     │  baseline / candidate A/B evaluation
   └──────────┬───────────┘
              │
       ┌──────┴──────┐
       │             │
   ┌───▼────┐   ┌────▼────┐
   │Promote │   │ Reject  │
   │+cooldn │   │+restore │
   └───┬────┘   └─────────┘
       │
       └─→ feeds back into the next task execution
```

### Key properties

- **Always evaluate before applying.** Candidates never overwrite the live skill library directly. The skill directory is snapshotted; the candidate is applied to an isolated copy; both baseline and candidate are evaluated; only the winner is promoted.
- **Regression-threshold gating.** Candidates whose metrics regress beyond `regression_threshold` are rejected. With `require_strict_improvement` enabled, parity with baseline is also a fail.
- **Cooldown.** A promoted skill enters a 24-hour cooldown by default to prevent rapid back-to-back changes.
- **One-click rollback.** `echo-agent evolution rollback <skill>` reverts the most recent promotion.
- **Full audit trail.** Trajectories, candidate skills, and evolution runs are all persisted; every candidate is traceable back to the source trajectory.
- **Human-in-the-loop.** With `auto_promote: false`, candidates land in `needs_review` and require explicit `evolution promote <id>`.

### Configuration (recommended production defaults)

Add the following to `echo-agent.yaml` to enable the evolution engine. For production, run with `auto_promote: false` for several rounds first to validate candidate quality before switching it on.

```yaml
evolution:
  enabled: true
  trigger_mode: "threshold"            # manual | threshold | scheduled
  threshold_trajectories: 50
  cron_expression: "0 4 * * *"
  max_candidates_per_run: 3
  max_trajectories_per_run: 200
  eval_dataset_path: "data/eval/baseline.yaml"
  regression_threshold: 0.05
  require_strict_improvement: true
  auto_promote: false                  # off by default in production; review manually
  candidate_review_required: true
  cooldown_seconds_after_promote: 86400
  trajectory_retention_days: 30
  skill_size_limit_bytes: 50000
  redact_args: true
```

### CLI

```bash
echo-agent evolution status              # engine status, pending candidates, last run
echo-agent evolution run                 # trigger a full evolution pass manually
echo-agent evolution list-candidates     # list candidates (--status pending|promoted|rejected|needs_review)
echo-agent evolution show-candidate <id> # candidate details: rationale, expected gain, A/B report
echo-agent evolution promote <id>        # manually promote a needs_review candidate
echo-agent evolution rollback <skill>    # revert the most recent promotion of a skill
echo-agent evolution init-dataset        # initialize the baseline evaluation dataset
```

Candidate format, scoring details, and rollback semantics live in `docs/evolution.md`.

---

## Configuration

Echo Agent loads configuration in this order: file passed via `-c` > `echo-agent.yaml` in the workspace > `~/.echo-agent/echo-agent.yaml`.

Minimal working configuration (credentials via environment variables):

```yaml
workspace: "~/.echo-agent"

models:
  defaultModel: "gpt-4o-mini"
  providers:
    - name: "openai"
      apiKeyEnv: "OPENAI_API_KEY"      # read from env, never hardcoded

channels:
  cli:
    enabled: true

permissions:
  adminUsers:
    - "cli_user"

evolution:
  enabled: true
  trigger_mode: "threshold"
  auto_promote: false
```

```bash
export OPENAI_API_KEY="sk-..."
echo-agent run -w .
```

> **Avoid committing API keys to `echo-agent.yaml`.** Use environment variables, local-only override files, or external secret managers (Vault, AWS Secrets Manager, etc.).

Supported providers: `openai`, `anthropic`, `gemini` / `google`, `bedrock` / `aws`, `openrouter`, plus any OpenAI-compatible endpoint. Model routing supports task-type matching, fallback strategies, and credential pool rotation.

Environment overrides use the `ECHO_AGENT_` prefix with double underscores between levels — for example `ECHO_AGENT_GATEWAY__PORT=9000`.

---

## Common commands

```bash
echo-agent                    # interactive CLI
echo-agent run                # run agent in foreground
echo-agent setup              # full configuration wizard (includes evolution sub-wizard)
echo-agent setup model        # configure models and providers
echo-agent setup channel      # configure messaging channels
echo-agent status             # show current configuration and runtime status
echo-agent gateway            # start the Gateway service
echo-agent eval -d eval.yaml  # run an evaluation dataset
```

Service management (Linux only):

```bash
echo-agent service install    # register systemd service
echo-agent service start
echo-agent service status
echo-agent service logs
echo-agent service uninstall
```

---

## Gateway API

The Gateway exposes Echo Agent over HTTP / WebSocket for custom frontends, internal systems, automation scripts, and external agent integrations. The root path `/` serves a built-in Playground for local debugging.

```bash
echo-agent gateway --host 127.0.0.1 --port 9000
```

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Built-in Playground |
| `GET` | `/api/v1/health` | Health check |
| `POST` | `/api/v1/message` | Send a message to the agent |
| `GET` | `/api/v1/sessions` | List sessions |
| `DELETE` | `/api/v1/sessions/{key}` | Reset a Gateway session |
| `POST` | `/api/v1/pair` | Generate a pairing code |
| `POST` | `/api/v1/pair/verify` | Verify a pairing code |
| `GET` | `/api/v1/stats` | Gateway runtime statistics |
| `GET` | `/ws` | WebSocket interface |
| `GET` | `/.well-known/agent.json` | A2A Agent Card |
| `POST` | `/a2a` | A2A JSON-RPC endpoint |

Authentication supports `open`, `allowlist`, and `pairing` modes. API tokens may be passed via `X-Echo-Agent-Token` or `Authorization: Bearer`. **Public deployments must enable authentication and network-level access control.**

---

## Channels

All channels normalize their input into a single message-event format and feed the same message bus and Agent Loop. Requests from CLI, Telegram, WeChat, QQBot, the Gateway, etc. share consistent session, memory, tool, and permission boundaries — and use the same continuously evolving skill library.

| Category | Channels |
|----------|----------|
| Local & system | `cli`, `webhook`, `cron` |
| International | `telegram`, `discord`, `slack`, `whatsapp`, `email`, `matrix` |
| China-region | `wechat`, `weixin`, `qqbot`, `feishu`, `dingtalk`, `wecom` |

Channel stability depends on third-party API policies and adapter implementation quality — see [Limitations](#limitations).

---

## Cognitive Memory System

Echo Agent's memory system manages the full lifecycle on top of two memory categories (user and environment), using a four-tier hierarchy to differentiate storage and retrieval across short- and long-term memories. The design provides time-sensitive, semantically verifiable persistence for user preferences, domain facts, and historical experience under bounded storage and a bounded context window.

### Memory categories

| Type | Description |
|------|-------------|
| User memory | Preferences, habits, communication style, personal context. Scoped to a session, or globally visible when tagged `global` |
| Environment memory | Project facts, tool configuration, process rules, domain knowledge. Globally visible, not scoped to sessions |

### Four tiers

| Tier | Description |
|------|-------------|
| Working | In-process buffer for the current conversation, capacity-limited (default 20), not persisted |
| Episodic | Summaries of conversation segments, indexed by session and time, persisted to SQLite |
| Semantic | Core facts distilled from episodes — the primary persistent layer, with CRUD and keyword + vector retrieval |
| Archival | Memories whose effective importance falls below threshold are auto-archived; further decay leads to deletion |

### Retrieval: hybrid BM25 + vector

`HybridRetriever` fuses BM25 keyword matching with FAISS vector similarity, adapting weights based on query entropy (Resonance Scoring): fuzzy queries lean on vector recall, precise queries lean on keywords. The Ebbinghaus decay factor is applied as a weight during the rerank stage; entries whose effective score drops below threshold are physically archived or deleted by a background task.

The vector index uses FAISS (optional dependency) with embeddings persisted in SQLite. Without FAISS installed, retrieval falls back to keyword-only.

### Forgetting curve

Adaptive decay follows the Ebbinghaus formula: `half_life = base × (1 + log₂(1 + access_count))`. The more often a memory is accessed, the longer its half-life and the slower it forgets. When effective importance drops below the archival threshold, the memory moves to Archival; below the forgetting threshold, it is deleted.

### Contradiction detection: temporal edges and belief revision

When new memories are written, they are checked against existing ones via a versioned memory lattice, supporting both LLM semantic verification and heuristic detection (same key, different content). **Contradictions are not silently overwritten — they are stored as temporal edges**, supporting belief revision and historical querying.

> Example: last week the user said "I have trypophobia"; today they say "I love staring at densely starred night skies". The system flags this as a potential conflict, records a temporal edge in the memory graph (instead of overwriting the older memory), and exposes the full belief-change history to the agent on subsequent related tasks.

### Consolidation and review

After a session ends, `MemoryConsolidator` uses an LLM to write the conversation summary into `HISTORY.md` and update long-term memory in `MEMORY.md`. The full sleep-time pipeline runs: create episodes → extract and promote semantic facts → contradiction detection → forgetting and archival sweep.

`MemoryReviewer` runs automatically after non-trivial conversations, asking an LLM whether user preferences, project facts, or lessons learned should be persisted, and performing add / replace / remove accordingly.

### Safety

All content written to memory passes through an injection scanner (prompt injection, role hijacking, credential exfiltration patterns) and an invisible-Unicode-character check. File writes use atomic replacement with cross-platform file locks to avoid corruption from concurrent writes.

---

## Tools and permissions

30+ built-in tools organized by category, all governed by a unified permission and approval system. MCP servers can dynamically register external tools via configuration.

| Category | Tools |
|----------|-------|
| Workspace | `read_file`, `write_file`, `edit_file`, `list_dir`, `search_files`, `patch` |
| Execution | `exec`, `execute_code`, `process` |
| Web | `web_fetch`, `web_search` |
| Collaboration | `message`, `notify`, `clarify`, `delegate_task`, `spawn_task` |
| Memory & sessions | `session_search`, `memory` |
| Tasks & workflow | `todo`, `task`, `workflow`, `cronjob` |
| Skills | `skills_list`, `skill_view`, `skill_manage`, `skill_install` |
| Multimodal | `vision_analyze`, `text_to_speech`, `image_generate` |
| Knowledge | `knowledge_search`, `knowledge_index` |
| Multi-agent | `agents_list`, `agents_route` |
| MCP | Dynamically registered from MCP servers in config |

High-risk tools (e.g. `exec`, `write_file`, `edit_file`) go through the approval flow by default; `permissions.adminUsers` and `permissions.approval` control access and approval policy. Approval supports LLM-based risk assessment (Smart Mode), path policies, and human admin review working together.

---

## Skills

Skills use an open directory + `SKILL.md` format. Built-in skills include `arxiv`, `weather`, `summarize`, `plan`, and `skill-creator`. They can be viewed, created, modified, deleted — and installed from local paths, Git repositories, or URLs.

The skill library supports automatic runtime evolution; see [Self-Evolution](#self-evolution).

---

## Architecture

A request enters through the CLI, Gateway, scheduler, webhook, or a channel adapter; it is normalized into a unified message event, routed through the message bus to the Agent Loop, and processed through model routing, memory retrieval, permission approval, tool execution, and observability — finally landing in the trajectory recorder, where it is consumed by the evolution engine.

```text
Channel / CLI / Gateway / Webhook / Cron
                ↓
          Message Bus
                ↓
          Agent Loop
                ↓
   Context Builder + Memory Retriever
                ↓
         Planner / Router
                ↓
        Permission Gate
                ↓
   Tool Execution / Model Call
                ↓
     Trajectory Recorder
                ↓
      Evolution Pipeline
```

![Architecture](https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/docs/assets/architecture.png)

### Code layout

```text
echo_agent/
├── a2a/            # A2A protocol (agent-to-agent interop)
├── agent/          # Agent loop, context building, compression, tool execution
├── bus/            # Message event queue
├── channels/       # CLI, messaging channels, webhook, cron adapters
├── cli/            # Setup wizard, status, service management, evolution sub-commands
├── config/         # Config schema, loader, defaults
├── evaluation/     # Evaluation dataset and runner (used by baseline / candidate A/B)
├── evolution/      # Self-evolution: trajectory recorder, evolver, promotion gate, scheduler
├── gateway/        # HTTP / WebSocket Gateway
├── knowledge/      # Knowledge index and retrieval
├── mcp/            # MCP client, transports, OAuth
├── memory/         # Four-tier memory, hybrid retrieval, forgetting curve, contradiction detection, vector index
├── models/         # Providers, routing, credential pool
├── observability/  # Health checks, spans, telemetry
├── permissions/    # Permission and credential primitives
├── plugins/        # Plugin hook system; the evolution module attaches via this
├── scheduler/      # Scheduled-job service
├── security/       # Risk classification, path policies, LLM safety approval
├── session/        # Session persistence
├── skills/         # Skill storage and review
├── storage/        # SQLite backend
└── tasks/          # Task management and workflow engine
```

---

## Limitations

Echo Agent is alpha software. Please understand these boundaries before relying on it:

- **Evolution quality depends on dataset coverage.** With an empty or undersized evaluation dataset, A/B comparisons become meaningless.
- **LLM-proposed candidate skills can be wrong.** For production, keep `auto_promote: false` and review before promoting.
- **Memory extraction can produce stale or inaccurate facts.** Verify against original context for critical decisions.
- **Shell, file editing, process control, and code execution are high-privilege tools.** Expose them only to trusted users; public endpoints must enable authentication.
- **Public Gateway deployments must enable `allowlist` or `pairing` authentication** alongside network-level access control (firewall / reverse proxy).
- **Channel adapters depend on third-party APIs.** Some adapters may be subject to bot policies or unofficial protocols, and stability is outside this project's control.
- **Internal storage schemas and configuration fields may change** before the stable release; read release notes when upgrading.

---

## Security recommendations

- Keep API keys, tokens, and `data/credentials.json` in environment variables or a secret manager — never commit them.
- For local development, prefer binding to `127.0.0.1`; before binding the Gateway to `0.0.0.0`, enable authentication.
- Shell, process, and code execution are high-privilege capabilities — restrict to trusted users.
- After enabling evolution, run with `auto_promote: false` for several rounds first; only enable auto-promotion once manual review has confirmed candidate quality.
- For troubleshooting, start with `echo-agent status` and `echo-agent evolution status`; in production, also check `echo-agent service logs`.

---

## Development

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

uv venv venv --python 3.11
source venv/bin/activate
uv pip install -e ".[all,dev]"

ruff check .
pytest
echo-agent run -w .
```

Before opening a PR:

- Run `ruff check .` and `pytest`
- Add or update tests for behavior changes
- Update docs for user-facing changes
- For larger changes to evolution, memory, permissions, storage, or tool execution, open an issue first

---

## Contributing

Contributions are welcome:

- **Bug reports.** File via [Issues](https://github.com/fuyuxiang/echo-agent/issues) with reproduction steps and logs.
- **New channel adapters or tools.** Look at existing implementations under `echo_agent/channels/` and `echo_agent/agent/tools/`.
- **Improving evolution quality.** Contribute evaluation datasets or report failure cases from the evolver.
- **Docs and examples.** Help fill out `docs/` and add to `examples/`.

A detailed contribution guide will live in `CONTRIBUTING.md` (work in progress).

---

## License

MIT
