<div align="center">

# Echo Agent

**An open-source AI Agent with cognitive memory and self-evolution**

Retains conversations, preferences and task experience across sessions, and continuously refines its own skills from real execution traces. Self-hosted on your laptop or your own servers.

[中文](README.md) · English

[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)
[![Python](https://img.shields.io/badge/Python-3.11%2B-blue.svg)](https://www.python.org/)
[![License](https://img.shields.io/badge/License-MIT-yellow.svg)](pyproject.toml)

<a href="https://github.com/fuyuxiang/echo-agent">
  <img src="docs/assets/echo-agent.png" alt="Echo Agent" width="720" />
</a>

</div>

---

## What is Echo Agent

Echo Agent is a self-hosted AI Agent in the same category as OpenClaw and Hermes Agent. On top of the standard Agent Loop it adds two layers most agents are missing: **cognitive memory that survives across sessions**, so the agent remembers conversation context, user preferences and task experience; and **self-evolution driven by real execution traces**, so the agent's skills keep improving as it runs, instead of being frozen at release time.

CLI, Gateway, Webhook, Cron and messaging channels — Telegram, Discord, Slack, WeCom, WeChat, QQ, Feishu, DingTalk and more — share the same message bus, Agent Loop, memory store and permission boundary. Sessions, memory, execution traces and local credentials live in the workspace directory by default, and tool calls go through a unified approval layer.

You can think of Echo Agent in four directions:

- Upward: connects to LLMs — main reasoning, context compression, embeddings and risk classification can each pick a different provider and model
- Outward: connects to messaging channels — 12 built-in channels feed the same Agent Loop through a unified message bus
- Inward: connects to cognitive memory — four memory tiers, decay, contradiction handling, and BM25 + vector hybrid retrieval
- Toward execution: connects to tools and protocols — built-in toolset, MCP client and A2A JSON-RPC under one approval layer

**In one sentence: Echo Agent is a long-running AI Agent that remembers what happened before and keeps improving from what it has done.**

It moves the agent past one-off answers — toward something that carries memory, skills and an auditable execution history, and works for you over time.

---

## Latest Updates

- **v0.1.0**
  - Self-evolution engine: a three-stage pipeline of trajectory recording, candidate generation and promotion review
  - Candidates are compared against the current version on an evaluation set before they take effect; any regression is rejected, with cooldown and one-click rollback
  - Four-tier cognitive memory: Working / Episodic / Semantic / Archival, with memory decay and importance-based reranking
  - Hybrid retrieval: BM25 keyword recall fused with FAISS vector recall, weighted adaptively per query
  - Contradiction handling: on conflicting writes both old and new values are kept with a temporal edge; reads return the latest, but the older value remains traceable
  - 12 messaging channels: CLI, Webhook, Cron, Telegram, Discord, Slack, WhatsApp, Email, Matrix, WeChat, QQ, Feishu, DingTalk, WeCom
  - Gateway HTTP / WebSocket APIs with a built-in Playground
  - A2A JSON-RPC endpoint and MCP client (with OAuth)
  - Smart approval: an LLM performs risk classification, combined with policy rules
  - Release notes: [CHANGELOG](https://github.com/fuyuxiang/echo-agent/releases)

---

## Why Echo Agent

For one-off Q&A a chat product is enough. Echo Agent solves a different problem: **an agent that needs to work across sessions and across processes, remembering what happened before and turning past results into better behavior**.

For an agent to actually work over the long run, all of the following have to hold — none of them is optional:

1. Execution must be **recorded in a structured form** — otherwise there is no way to look back and no way to tell whether a change improved or hurt anything; memory must be **layered and time-decaying** — the context window cannot fit the full history, but naive truncation throws away what matters
2. Skill updates must be **validated before they take effect** — letting an LLM rewrite skill definitions directly produces visible regressions; high-risk tool calls must go through **a unified approval gate** — once `exec` or `write_file` is approved on a wrong premise it is hard to undo; multiple entry points (CLI, Gateway, messaging channels) must **share the same state** — otherwise memory and permission boundaries fragment

Echo Agent fills in these missing pieces in one place, so an agent can actually keep running in real environments.

---

## Core Capabilities

| Module | Description |
|--------|-------------|
| Agent Loop | The unified loop that receives events, builds context, calls the model and executes tools — shared by every entry point |
| Cognitive memory | Four tiers (Working / Episodic / Semantic / Archival) with decay, contradiction handling and importance reranking |
| Hybrid retrieval | BM25 keyword recall fused with FAISS vector recall, weighted adaptively per query; degrades gracefully when FAISS is unavailable |
| Self-evolution | Three-stage pipeline of trajectory recording → candidate generation → promotion review; candidates must not regress on the eval set, with cooldown and rollback |
| Unified entry points | CLI, Gateway, Webhook, Cron and 12 messaging channels normalize into a single event type on the message bus |
| Model routing | Per-task-type routing: main reasoning, context compression, embeddings and risk approval each get their own provider and model |
| Tool approval | Three modes (`manual` / `smart` / `off`); unattended channels can use `unattendedPolicy: deny` to default-deny |
| Toolset | Workspace files, shell execution, web, collaboration, memory, tasks, skills, multimodal, knowledge, plus dynamic MCP registration |
| Cross-process interop | A2A JSON-RPC (`/a2a` + `/.well-known/agent.json`) and an MCP client with OAuth |
| Credentials & redaction | Encrypted local credential store; tool logs redact fields named `key`, `token`, `secret` and similar |
| Evaluation framework | Eval datasets and a runner; evolution candidates can only be promoted if they do not regress against the current version |
| Local-first | Sessions, memory, traces, skill candidates and audit logs live in the workspace by default; credentials can be encrypted at rest |

---

## Architecture

Entry points, the runtime core and the improvement loop form one path: the message bus normalizes events from every entry, the Agent Loop calls models and tools within memory and permission boundaries, execution traces flow into the self-evolution pipeline, and updated skills feed back into subsequent runs.

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent architecture" width="820" />
</div>

---

## Use Cases

- You want the agent to run on your own laptop or servers, with full audit and traceability over tool execution
- You want conversations, preferences, project facts and task experience to accumulate across sessions instead of being re-explained each time
- You want the agent's skills to keep improving from real usage, instead of staying at the shipped version
- You want multiple entry points (CLI, Webhook, messaging bots) to share the same memory and permission boundary
- You want strict approval on high-risk tools like `exec` and `write_file` to keep mistakes from spreading
- You want to plug in several model providers at once and pick the right model per task type

---

## Quick Start

### Option 1: From source

Requirements: Python **3.11+**, Linux / macOS / WSL2, and at least one model provider API key. [`uv`](https://docs.astral.sh/uv/) is recommended for managing virtual environments.

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

uv venv venv --python 3.11
source venv/bin/activate

uv pip install -e ".[all]"
```

Configure a model provider and start:

```bash
export OPENAI_API_KEY="sk-..."

echo-agent setup -w .
echo-agent run -w .
```

PowerShell:

```powershell
$env:OPENAI_API_KEY = "sk-..."
echo-agent run -w .
```

CMD:

```cmd
set OPENAI_API_KEY=sk-...
echo-agent run -w .
```

### Option 2: Install script

Recommended for local development only:

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh
bash install.sh
```

The script writes to `PATH` and registers a systemd service on Linux. For production deployments, prefer the from-source path and manage the virtual environment, credentials and service registration yourself.

---

## Documentation

| Doc | Description | Link |
|-----|-------------|------|
| Quick Start | Install, configure and first run | [#quick-start](#quick-start) |
| Self-evolution | Trajectories, evolver, promotion review and config examples | [docs/evolution.md](docs/evolution.md) |
| Memory | Four-tier structure, hybrid retrieval, decay and contradiction handling | [docs/memory.md](docs/memory.md) |
| Gateway | HTTP / WebSocket APIs, auth modes and A2A endpoints | [docs/gateway.md](docs/gateway.md) |
| Channels | Setup notes for the 12 messaging channels | [docs/channels.md](docs/channels.md) |
| Tools & Skills | Built-in tools, MCP integration, skill authoring | [docs/skills.md](docs/skills.md) |
| Security & Privacy | Permission model, approval policies, credential management | [docs/security.md](docs/security.md) |

Full docs: `https://github.com/fuyuxiang/echo-agent/tree/master/docs`

---

## Development & Contributing

### Local development

```bash
uv pip install -e ".[all,dev]"

ruff check .
pytest
echo-agent run -w .
```

Before opening a PR, make sure `ruff check .` and `pytest` pass, and keep `README.md` (Chinese, primary) and `README.en.md` (English) in sync.

### Community

- Design discussion, usage questions and roadmap: [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions)
- Good entry points: channel adapters, built-in tools, MCP integrations, skill examples, eval datasets, documentation, deployment templates

### Issues

- Bugs and feature requests: [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues)
- For security issues, please file privately via the `security` issue template instead of posting reproductions publicly

---

## License & Credits

### License

MIT License. The license declaration lives in `pyproject.toml`.

### Inspiration

- [Anthropic Claude Code](https://github.com/anthropics/claude-code) — Agent Loop and tool approval model
- [OpenClaw](https://github.com/openclaw/openclaw) — multi-channel adaptation and sandbox backends
- [Hermes Agent](https://github.com/hermes/hermes-agent) — model routing and trajectory export
- [MCP](https://modelcontextprotocol.io/) — tool protocol standard
- [A2A](https://github.com/google/a2a) — cross-process Agent interop protocol

### Tech credits

Built on top of `pydantic`, `aiohttp`, `aiosqlite`, `numpy`, `faiss-cpu`, `tiktoken`, `croniter`, `loguru` and other community projects; provider clients use `openai`, `anthropic`, `google-generativeai` and `boto3`. Thanks to all the maintainers.

---

<div align="center">

**An open-source AI Agent with cognitive memory and self-evolution**

<a href="https://github.com/fuyuxiang/echo-agent">GitHub</a> ·
<a href="https://gitee.com/fuyuxiang/echo-agent">Gitee</a> ·
<a href="https://github.com/fuyuxiang/echo-agent/issues">Issues</a> ·
<a href="https://github.com/fuyuxiang/echo-agent/tree/master/docs">Full docs</a>

</div>

