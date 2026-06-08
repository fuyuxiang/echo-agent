<div align="center">

# Echo Agent

**An AI Agent that remembers the past and learns for the future**

<a href="https://github.com/fuyuxiang/echo-agent">
  <img src="docs/assets/echo-agent.png" alt="Echo Agent" width="720" />
</a>

<br/>

[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)

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
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all]"

# Configure and run
export OPENAI_API_KEY="sk-..."
echo-agent setup -w .
echo-agent run -w .
```

<details>
<summary>China mirror / Windows / one-liner script</summary>

```bash
# Aliyun mirror
uv pip install -e ".[all]" -i https://mirrors.aliyun.com/pypi/simple/

# Gitee mirror
git clone https://gitee.com/fuyuxiang/echo-agent.git
```

```powershell
# PowerShell
$env:OPENAI_API_KEY = "sk-..."
echo-agent run -w .
```

```bash
# One-liner install script (local dev only)
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
less install.sh && bash install.sh
```

</details>

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

```bash
uv pip install -e ".[all,dev]"
ruff check .
pytest
```

Please ensure lint and tests pass before submitting a PR, and keep both Chinese and English READMEs in sync.

**Good entry points:** channel adapters · built-in tools · MCP integrations · skill examples · eval datasets · documentation · deployment templates

**Community:**
- [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) — design discussion, usage questions
- [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) — bugs and feature requests

---

## License

MIT License — see [pyproject.toml](pyproject.toml)

---


## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=fuyuxiang/echo-agent&type=Date)](https://star-history.com/#fuyuxiang/echo-agent&Date)