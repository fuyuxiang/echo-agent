<div align="center">

# Echo Agent

**A self-hosted, long-running agent runtime — skills evolve from runtime trajectories, memory persists beyond the session window.**

[中文](README.md) · English

[![Python](https://img.shields.io/badge/python-3.11%2B-3776AB.svg?logo=python&logoColor=white)](https://www.python.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](pyproject.toml)
[![Status](https://img.shields.io/badge/status-alpha-f59e0b.svg)](#project-status)
[![Self-hosted](https://img.shields.io/badge/self--hosted-✓-111827.svg)](#architecture)

[Quickstart](#quickstart) · [Self-Evolution](#self-evolution) · [Memory](#memory) · [Architecture](#architecture)

</div>

---

## Overview

Echo Agent is an agent runtime built for private deployments. Aside from model API calls, trajectories, memory, sessions, and credentials remain on the local machine — no telemetry is sent to external services by default.

Unlike most agent frameworks that treat a single tool-calling loop as the end goal, Echo Agent treats every task execution as a learnable sample:

- **Runtime improvement loop.** Tasks are written to a persistent layer as structured Trajectories. The Evolver consumes failing and low-scoring trajectories and uses an LLM to propose candidate skill changes. Candidates are first compared against the baseline through an A/B run on an evaluation dataset, and are promoted into the skill library only when metrics strictly outperform the baseline. Promotions can be reverted with a single rollback command.
- **Tiered memory.** A four-tier hierarchy (Working / Episodic / Semantic / Archival), Ebbinghaus adaptive forgetting, hybrid BM25 + vector retrieval, and contradiction detection on a versioned memory lattice — together giving memory time-sensitivity and verifiability.
- **Single source of truth across entry points.** CLI, webhook, cron, gateway, and the messaging-channel adapters share the same message bus, Agent Loop, memory, and permission boundary.
- **Multiple model providers.** OpenAI, Anthropic, Google Gemini, AWS Bedrock, OpenRouter, and any OpenAI-compatible endpoint.

> **Current stage:** Alpha. Configuration fields, internal storage schema, and APIs may change before a stable release.

---

## Motivation

Most agent frameworks freeze their capability boundary at deployment: failure modes observed at runtime cannot flow back into skill definitions, and the hard truncation of a context window is not, by itself, long-term memory. Echo Agent rebuilds its primitives around two long-overlooked questions:

1. **Does capability grow with runtime?**
2. **Does memory persist beyond the session boundary?**

The first is handled by the self-evolution engine — record, reflect, propose, evaluate, promote, cooldown. The second is handled by the tiered memory system — layered storage, adaptive decay, hybrid retrieval, contradiction detection, and sleep-time consolidation. Together they form the system's main feedback loop: memory supplies high-quality samples to the evolution engine, and the evolution engine improves trajectory quality and skill-library utility.

---

## Features

| Module | Description |
|--------|-------------|
| **Self-evolving skill library** | Trajectory → Evolver → A/B evaluation → promote or reject, with cooldown and rollback |
| **Four-tier memory** | Working / Episodic / Semantic / Archival, with hybrid retrieval and adaptive forgetting |
| **Multi-agent collaboration** | Worker profiles (planner / coder / researcher / operator) routed by task, parallel execution |
| **Unified message bus** | CLI, webhook, cron, gateway, and channel adapters share one Agent Loop |
| **Fine-grained permissions** | High-risk tools enter an approval flow, with optional LLM risk scoring, path policy, and admin review |
| **A2A + MCP** | Implements the A2A protocol; integrates Anthropic MCP for mounting external MCP-server tools |
| **Multiple LLM providers** | OpenAI / Anthropic / Gemini / Bedrock / OpenRouter, plus any OpenAI-compatible endpoint |
| **Self-hosted** | Trajectories, memory, sessions, and credentials persist on the local filesystem and SQLite |

---

## Quickstart

### Requirements

- Python **3.11+**
- Linux, macOS, or WSL2
- An API key for at least one model provider
- [`uv`](https://docs.astral.sh/uv/) is recommended for environment management

### Install from source

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

uv venv venv --python 3.11
source venv/bin/activate
uv pip install -e ".[all]"

echo-agent setup -w .   # interactive configuration wizard
echo-agent run -w .     # foreground run
```

### Install script (development environments only)

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh         # please review before executing
bash install.sh
```

> The script writes a symlink onto `PATH` and (on Linux) registers a systemd service. For production-style deployments, prefer the source install path and manage the virtualenv and service registration yourself.

### First run

```bash
# Launch the interactive CLI
echo-agent

# Submit a task at the prompt
> write a Python script that monitors free disk space and save it as disk_check.py

# After the task ends, the trajectory is persisted to SQLite for later evolution to consume
echo-agent evolution status
```

---

## Self-Evolution

> Turn each task execution into a verifiable improvement signal. Candidates do not take effect directly: evaluate first, then promote, with the option to roll back.

### Pipeline

```text
   ┌──────────────────────┐
   │  TrajectoryRecorder  │  capture full trajectory of every Agent Loop run
   └──────────┬───────────┘
              ▼
   ┌──────────────────────┐
   │       Evolver        │  LLM proposes candidate skill changes from trajectories
   └──────────┬───────────┘
              ▼
   ┌──────────────────────┐
   │    PromotionGate     │  baseline / candidate A/B evaluation
   └──────────┬───────────┘
              │
       ┌──────┴──────┐
       ▼             ▼
   ┌────────┐   ┌─────────┐
   │Promote │   │ Reject  │
   │+cooldn │   │+restore │
   └───┬────┘   └─────────┘
       │
       └─→ feeds back into the next task execution
```

### Key constraints

- **Always evaluate before applying.** Candidates never overwrite the live skill library directly. The skill directory is snapshotted, the candidate is applied to an isolated copy, and both baseline and candidate are evaluated; only the winner is promoted.
- **Regression-threshold gating.** Candidates whose metrics regress beyond `regression_threshold` are rejected. With `require_strict_improvement` enabled, parity with the baseline is also a fail.
- **Cooldown.** A promoted skill enters a 24-hour cooldown by default to prevent rapid back-to-back changes.
- **One-click rollback.** `echo-agent evolution rollback <skill>` reverts the most recent promotion.
- **Full audit trail.** Trajectories, candidate skills, and evolution runs are persisted; every candidate is traceable back to its source trajectories.
- **Human-in-the-loop.** With `auto_promote: false`, candidates land in `needs_review` and require explicit `evolution promote <id>`.

### Configuration

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
echo-agent evolution list-candidates     # --status pending|promoted|rejected|needs_review
echo-agent evolution show-candidate <id> # rationale, expected gain, A/B report
echo-agent evolution promote <id>        # manually promote a needs_review candidate
echo-agent evolution rollback <skill>    # revert the most recent promotion of a skill
echo-agent evolution init-dataset        # initialize the baseline evaluation dataset
```

Full design notes for candidate format, scoring, and rollback semantics: TODO (`docs/` currently only contains the architecture diagram).

---

## Memory

A four-tier hierarchy that differentiates storage and retrieval across short- and long-term memory, with the goal of providing time-sensitive, semantically verifiable persistence for user preferences, domain facts, and historical experience under bounded storage and a bounded context window.

### Tiers

| Tier | Purpose | Persistence |
|------|---------|-------------|
| **Working** | In-process buffer for the current conversation, capacity-limited (default 20) | No |
| **Episodic** | Summaries of conversation segments, indexed by session and time | SQLite |
| **Semantic** | Core facts distilled from episodes — the primary persistent layer | SQLite + vector index |
| **Archival** | Memories whose effective importance falls below threshold are auto-archived; further decay leads to deletion | SQLite |

### Retrieval

`HybridRetriever` fuses BM25 keyword matching with FAISS vector similarity, adapting weights based on query entropy ("Resonance Scoring"): fuzzy queries lean on vector recall, precise queries lean on keywords. The Ebbinghaus decay factor is applied as a weight during the rerank stage.

The vector index uses FAISS (optional dependency); without FAISS installed, retrieval falls back to keyword-only.

### Forgetting curve

Adaptive decay follows the Ebbinghaus formula:

```
half_life = base × (1 + log₂(1 + access_count))
```

The more often a memory is accessed, the longer its half-life and the slower it forgets. When effective importance drops below the archival threshold, the memory moves to Archival; below the forgetting threshold, it is deleted.

### Contradiction detection

When a new memory is written, it is checked against existing memory via a versioned memory lattice, with both LLM verification and a heuristic mode (same key, different content). **Contradictions are not silently overwritten** — they are stored as temporal edges, preserving the full belief-change history.

### Consolidation and review

After a session ends, `MemoryConsolidator` runs: write summaries → create episodes → extract and promote semantic facts → contradiction detection → forgetting and archival sweep. `MemoryReviewer` runs after non-trivial conversations and asks an LLM whether user preferences, project facts, or lessons learned should be persisted, performing add / replace / remove accordingly.

### Memory categories

| Type | Description |
|------|-------------|
| **user** | Preferences, habits, communication style, personal context. Scoped to a session, or globally visible when tagged `global` |
| **environment** | Project facts, tool configuration, process rules, domain knowledge. Globally visible |

---

## Configuration

Echo Agent loads configuration in this order: file passed via `-c` > `echo-agent.yaml` in the workspace > `~/.echo-agent/echo-agent.yaml`.

### Minimal working configuration

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

> Avoid committing API keys to `echo-agent.yaml`. Use environment variables, local-only override files, or external secret managers (Vault, AWS Secrets Manager, etc.).

### Providers and routing

Supported providers: `openai`, `anthropic`, `gemini` / `google`, `bedrock` / `aws`, `openrouter`, plus any OpenAI-compatible endpoint. Model routing supports task-type matching, fallback strategies, and credential-pool rotation.

### Environment variables

`ECHO_AGENT_`-prefixed variables are split on double underscores `__` into nested config keys at runtime.

| Name | Required | Default | Description |
|------|----------|---------|-------------|
| `ECHO_AGENT_CREDENTIAL_KEY` | No | unset | Symmetric key used to encrypt the local credential store. Required when `credentials.requireEncryption: true` (the default) is in effect. The variable name is configurable via `credentials.encryptionKeyEnv` |
| `ECHO_HOME` | No | `~/.echo-agent` | Workspace root used by the install script and default runtime |
| `ECHO_INSTALL_DIR` | No | `$ECHO_HOME/echo-agent` | Source-clone directory used by the install script |
| `ECHO_COMMAND_LINK_DIR` | No | `~/.local/bin` or `/usr/local/bin` | Directory the install script writes the `echo-agent` symlink into |
| `ECHO_AGENT_<SECTION>__<KEY>` | No | — | Arbitrary config override, e.g. `ECHO_AGENT_GATEWAY__PORT=9000` |

Provider API keys are read from `models.providers[].apiKey` (not recommended in plaintext) or from each provider SDK's standard environment variable (e.g. `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`). The repository does not ship a `.env.example`: TODO list the canonical environment variable names per provider.

---

## Commands

```bash
echo-agent                    # interactive CLI
echo-agent run                # foreground run
echo-agent setup              # full configuration wizard (includes evolution sub-wizard)
echo-agent setup model        # configure models and providers
echo-agent setup channel      # configure messaging channels
echo-agent status             # show current configuration and runtime status
echo-agent gateway            # start the Gateway service
echo-agent eval -d eval.yaml  # run an evaluation dataset
echo-agent plugin list        # list loaded plugins
```

Service management (Linux only):

```bash
echo-agent service install
echo-agent service start
echo-agent service status
echo-agent service logs
echo-agent service uninstall
```

---

## Gateway

The Gateway exposes Echo Agent over HTTP / WebSocket for custom frontends, internal systems, automation scripts, and external agent integrations. The root path serves a built-in Playground for local debugging.

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
| `GET` | `/.well-known/agent.json` | A2A Agent Card (when `a2a.enabled`) |
| `POST` | `/a2a` | A2A JSON-RPC endpoint (when `a2a.enabled`) |

Authentication supports `open`, `allowlist`, and `pairing` modes. API tokens may be passed via `X-Echo-Agent-Token` or `Authorization: Bearer`. **Public deployments must enable authentication and network-level access control.**

---

## Channels

All channels normalize their input into a single message-event format and feed the same message bus and Agent Loop. Requests from the CLI, Telegram, WeChat, QQBot, the Gateway, and other adapters share consistent session, memory, tool, and permission boundaries — and use the same continuously evolving skill library.

| Category | Channels |
|----------|----------|
| Local & system | `cli`, `webhook`, `cron` |
| International | `telegram`, `discord`, `slack`, `whatsapp`, `email`, `matrix` |
| China-region | `wechat` / `weixin`, `qqbot`, `feishu`, `dingtalk`, `wecom` |

Channel stability depends on third-party API policies and adapter quality — see [Limitations](#limitations).

---

## Tools

Built-in tools are organized by category and governed by a unified permission and approval system. MCP servers can dynamically register external tools via configuration.

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
| MCP | Dynamically registered from MCP servers in config |

High-risk tools (`exec`, `write_file`, `edit_file`, etc.) go through the approval flow by default; `permissions.adminUsers` and `permissions.approval` control access and approval policy. Approval supports LLM-based risk assessment (Smart Mode), path policies, and human admin review working together.

---

## Skills

Skills use an open directory + `SKILL.md` format. Built-in skills include `arxiv`, `weather`, `summarize`, `plan`, and `skill-creator`. They can be viewed, created, modified, deleted — and installed from local paths, Git repositories, or URLs.

The skill library supports automatic runtime evolution; see [Self-Evolution](#self-evolution).

---

## Architecture

A request enters through the CLI, gateway, scheduler, webhook, or a channel adapter; it is normalized into a unified message event, routed through the message bus to the Agent Loop, and processed through model routing, memory retrieval, permission approval, tool execution, and observability — finally landing in the trajectory recorder, where the evolution engine consumes it.

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

> TODO: `docs/assets/architecture.png` is the existing architecture image — verify it still reflects the codebase as new modules (e.g. evolution, plugins) land.

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

## Project Status

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

## Security Model

Echo Agent is designed primarily as a developer-local tool with broad access to the workspace and the host machine. Operators should understand the following surface area before exposing it to outside channels:

**What the agent can access**

- Filesystem: `read_file` / `write_file` / `edit_file` / `list_dir` / `search_files` / `patch` operate on the configured workspace by default. `tools.restrictToWorkspace` and `tools.safeWriteRoot` can narrow the writable area further.
- Shell and processes: `exec` runs against `tools.exec.host` (default `sandbox`) and is gated by `safeBins` / `allowedCommands` / `blockedCommands`. `process` and `execute_code` provide process management and code execution.
- Network: `web_fetch` / `web_search` make outbound HTTP requests. The web tool is enabled by default; `execution.networkPolicy` can be set to `deny` or `restricted`.
- Model API: conversations, tool arguments, and context summaries from each trajectory are sent to the configured model providers.

**Credentials and secrets**

- API keys and tokens come from `models.providers[].apiKey`, environment variables, or the credential store (encrypted by the symmetric key referenced by `credentials.encryptionKeyEnv`).
- The repository does not ship a `.env.example`. Do not commit secrets in `echo-agent.yaml`.
- Tool-call logs redact arguments whose key names contain `key`, `token`, `secret`, `password`, `api_key`, `credential`, or `auth` before persisting (see `echo_agent/agent/tools/registry.py`).

**Permissions and approvals**

- Tools listed in `permissions.approval.requireApproval` enter the approval flow. Defaults: `cronjob`, `exec`, `execute_code`, `process`, `skill_install`, `skill_manage`.
- `permissions.approval.mode` supports `manual`, `smart` (LLM risk classification), and `off`.
- `permissions.approval.unattendedPolicy` controls the default behavior on unattended channels (e.g. webhook): allow safe operations or deny.
- `permissions.adminUsers` lists users allowed to manage approvals and high-privilege tools. For CLI use, `cli_user` is typically included.

**Network entry points**

- The Gateway (HTTP / WebSocket) defaults to `auth.mode: allowlist`. Public deployments must use `allowlist` or `pairing`, combined with firewall / reverse-proxy controls.
- Each messaging channel (Telegram, Slack, QQBot, WeChat, etc.) narrows the set of accepted senders via its `allow_from` list.

**Known risks**

- LLM prompt injection: instructions embedded in external content (web pages, files, messages) may try to manipulate agent behavior. The `memory` write path includes an injection scanner and an invisible-Unicode check, but other tool outputs are not uniformly filtered.
- Tool misuse: once authorized, write- and execute-class tools can modify files or run commands under faulty assumptions; keep approvals and audit trails enabled.
- Credential exfiltration: log redaction covers common sensitive field names but does not extend to model prompts or responses; do not paste production secrets into the conversation.
- Self-evolution candidates can be wrong: `auto_promote` defaults to `true`, so production deployments should set it to `false` and review manually.

The repository does not claim a completed security audit or third-party penetration test. **It should not be treated as a production-grade security baseline out of the box.**

---

## Privacy

- **Local data.** By default stored under `workspace/data/` (governed by `storage.databasePath` and `storage.*Dir`): the SQLite database (sessions, memory, trajectories, skill candidates), `memory/HISTORY.md` and `memory/MEMORY.md`, `logs/`, `media_cache/`, and similar.
- **Remote data.** Aside from model provider APIs, outbound web-tool requests, and (when configured) the OpenTelemetry exporter, the runtime does not push data outbound. Model providers receive conversation content, tool definitions, and tool arguments.
- **Observability.** `observability.otelEnabled` defaults to `true`, but trace export only happens when `otelEndpoint` is set; without an endpoint, telemetry stays local.
- **Deleting local state.** After stopping the service, removing `workspace/data/` clears persistent state. You can also remove subdirectories individually (`data/echo_agent.db`, `data/memory/`, `data/sessions/`, etc.).
- **Audit.** Evaluation runs, evolution runs, and multi-agent delegation write to audit files such as `data/delegation_audit.jsonl` for after-the-fact tracing.

> Privacy guarantees ultimately depend on how operators deploy the system and on the policies of the chosen providers. The repository itself does not provide privacy guarantees.

---

## Limitations

- **Evolution quality depends on dataset coverage.** With an empty or undersized evaluation dataset, A/B comparisons become meaningless.
- **LLM-proposed candidate skills can be wrong.** For production, keep `auto_promote: false` and review before promoting.
- **Memory extraction can produce stale or inaccurate facts.** Verify against the original context for critical decisions.
- **Shell, file editing, process control, and code execution are high-privilege tools.** Expose them only to trusted users; public endpoints must enable authentication.
- **Public Gateway deployments must enable `allowlist` or `pairing` authentication** alongside network-level access control (firewall / reverse proxy).
- **Channel adapters depend on third-party APIs.** Some adapters may be subject to bot policies or unofficial protocols, with stability outside this project's control.
- **Internal storage schema and configuration fields may change** before the stable release; read release notes when upgrading.

---

## Operational recommendations

- Keep API keys, tokens, and `data/credentials.json` in environment variables or a secret manager — never commit them.
- For local development, prefer binding to `127.0.0.1`; before binding the Gateway to `0.0.0.0`, enable authentication.
- After enabling evolution, run with `auto_promote: false` for several rounds first; only enable auto-promotion once manual review has confirmed candidate quality.
- For troubleshooting, start with `echo-agent status` and `echo-agent evolution status`; on Linux, `echo-agent service logs` is also available.

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

The repository ships `ruff` and `pytest` in the `dev` extra. There is no dedicated typecheck or formatter command beyond `ruff`. There is no Dockerfile or `docker-compose.yml`. Tests live under `tests/` (~70 files at the time of writing); unit tests for evolution, memory, gateway, channels, multi-agent, planning, and tool execution are present.

Before opening a PR:

- Run `ruff check .` and `pytest`
- Add or update tests for behavior changes
- Update both `README.md` (Chinese, canonical) and `README.en.md` (English) when user-visible behavior changes
- For larger changes to evolution, memory, permissions, storage, or tool execution, open an issue first

---

## Contributing

Contributions are welcome:

- **Bug reports.** File via [Issues](https://github.com/fuyuxiang/echo-agent/issues) with reproduction steps and logs.
- **New channel adapters or tools.** Look at existing implementations under `echo_agent/channels/` and `echo_agent/agent/tools/`.
- **Improving evolution quality.** Contribute evaluation datasets or report failure cases from the evolver.
- **Docs and examples.** Help fill out `docs/`.

A detailed contribution guide is not yet in the repository — `CONTRIBUTING.md`: TODO.

---

## License

`pyproject.toml` declares the project as MIT-licensed.

> TODO: a `LICENSE` file is not yet committed at the repository root and should be added.
