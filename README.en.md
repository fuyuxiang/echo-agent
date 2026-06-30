<div align="center">

# Echo Agent

**An AI Agent that remembers the past and learns for the future**

<a href="https://github.com/fuyuxiang/echo-agent">
  <img src="docs/assets/echo-agent.png" alt="Echo Agent" width="720" />
</a>

<br/>

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Python](https://img.shields.io/pypi/pyversions/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)
[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)

[中文](README.md) · English

</div>

---

## What is Echo Agent

Echo Agent is a self-hosted, long-running AI Agent. Unlike one-off Q&A, it can:

- **Cross-session memory** — Four-tier cognitive memory with automatic decay and contradiction detection, solving memory explosion in long-running scenarios. Conversations never start from scratch.
- **Self-evolving skills** — Generates improvement candidates from real execution traces, validated against an eval set before taking effect. Supports rollback.
- **Unified multi-entry** — CLI, Gateway, Webhook, Cron and 12 messaging channels (Telegram / Discord / Slack / WeChat / Feishu / DingTalk etc.) share one state.
- **Safe and auditable** — High-risk tool calls go through unified approval, credentials are encrypted at rest, execution logs are fully auditable.

In one sentence: **An agent that carries memory and ever-improving skills, working for you over time.**

---

## Quick Start

Requirements: Python 3.11+, at least one model API key.

```bash
# Install
pip install "echo-agent[all]"

# Configure a model API key (any one of these)
export OPENAI_API_KEY="sk-..."
# ANTHROPIC_API_KEY / GOOGLE_API_KEY / OPENROUTER_API_KEY are also supported,
# as well as AWS Bedrock (via standard AWS credentials)

# Interactive setup wizard (data lives in ~/.echo-agent by default)
echo-agent setup

# Run
echo-agent run
```

> Inspect configuration with the CLI: `echo-agent config explain <key>` for a single option (description, type, default and allowed values), `echo-agent config dump` to view the active configuration (secrets are redacted), and `echo-agent config validate` to check a config file.

<details>
<summary>China mirror / Windows / one-liner script</summary>

```bash
# Aliyun PyPI mirror
pip install "echo-agent[all]" -i https://mirrors.aliyun.com/pypi/simple/
```

```powershell
# Windows (PowerShell)
pip install "echo-agent[all]"
$env:OPENAI_API_KEY = "sk-..."
echo-agent setup
echo-agent run
```

```bash
# One-liner install script (Linux / macOS / WSL2 only; installs from source
# into ~/.echo-agent and can register a systemd service — review before running)
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh && bash install.sh
```

</details>

> Resident gateway: start it in the foreground with `echo-agent gateway` (or have systemd/launchd manage it — `echo-agent service install` registers a systemd service). Once the gateway is running, you can attach on the same machine with `echo-agent cli` as a thin client, opening a separate session that talks to the same resident agent. Local loopback only (127.0.0.1); remote addresses are not supported — use ssh for remote access.

---

## Architecture

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent Architecture" width="820" />
</div>

---

## Core Capabilities

| Module | Description |
|--------|-------------|
| **Agent Loop** | Receive events → build context → call model → execute tools, shared across all entry points |
| **Cognitive Memory** | Working / Episodic / Semantic / Archival four tiers, with decay, contradiction detection and importance reranking |
| **Hybrid Retrieval** | BM25 + FAISS vector fusion, adaptive weighting per query, graceful degradation without FAISS |
| **Self-Evolution** | Trajectory recording → candidate generation → eval comparison → promote/reject, with cooldown and rollback |
| **Model Routing** | Main reasoning, context compression, embeddings and risk approval each configurable with independent provider and model |
| **Tool Approval** | Three modes: `manual` / `smart` / `off`, unattended channels default to denying high-risk calls |
| **Cross-Process Interop** | A2A JSON-RPC + MCP client (with OAuth), dynamic tool registration |
| **Local-First** | Sessions, memory, traces and credentials stored in the workspace by default, credentials encrypted at rest |

---

## Use Cases

- Run the agent on your own machine/server with full audit trail
- Retain conversations, preferences and task experience across sessions
- Let agent skills improve from real usage, not frozen at release
- Share memory and permissions across multiple entry points (CLI, Webhook, bots)
- Enforce approval for high-risk tools to prevent accidental damage
- Use multiple model providers simultaneously, routed by task type

---

## Development & Contributing

Install from source (development mode):

```bash
git clone https://github.com/fuyuxiang/echo-agent.git   # mirror: https://gitee.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all,dev]"

# Pre-submit checks
ruff check .
pytest
```

Please ensure lint and tests pass before submitting a PR (CI runs the same checks on every PR), and keep both Chinese and English READMEs in sync.

**Good entry points:** channel adapters · built-in tools · MCP integrations · skill examples · eval datasets · documentation · deployment templates

**Community:**
- [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) — design discussion, usage questions
- [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) — bugs and feature requests

---

## License

[MIT License](LICENSE)

---


## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=fuyuxiang/echo-agent&type=Date)](https://star-history.com/#fuyuxiang/echo-agent&Date)
