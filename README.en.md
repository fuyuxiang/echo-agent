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
- **Unified multi-entry** — CLI, Gateway, Webhook, Cron and 14 channels in total (Telegram / Discord / Slack / WeChat / WeCom / Feishu / DingTalk / QQ / WhatsApp / Email / Matrix) share one state.
- **Safe and auditable** — High-risk tool calls go through unified approval, credentials are encrypted at rest, execution logs are fully auditable.

In one sentence: **An agent that carries memory and ever-improving skills, working for you over time.**

---

## Quick Start

Requirements: Python 3.11+, at least one model API key.

```bash
# Install
pip install "echo-agent[all]"

# Interactive setup wizard (prompts for your model API key; data lives in ~/.echo-agent by default)
echo-agent setup

# Run an interactive conversation
echo-agent run
```

<details>
<summary>China mirror / Windows / one-liner script</summary>

```bash
# Aliyun PyPI mirror
pip install "echo-agent[all]" -i https://mirrors.aliyun.com/pypi/simple/
```

```powershell
# Windows (PowerShell)
pip install "echo-agent[all]"
echo-agent setup
echo-agent run
```

```bash
# One-liner install script (Linux / macOS / WSL2 only; installs from source
# into ~/.echo-agent and can register a background service — review before running)
# From GitHub:
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
# From the Gitee mirror (faster inside mainland China):
curl -fsSL -o install.sh https://gitee.com/fuyuxiang/echo-agent/raw/master/scripts/install.sh

less install.sh && bash install.sh

# The script probes GitHub and the Gitee mirror and clones from whichever
# answers faster. Override it with --repo github or --repo gitee.
#
# --repo covers the git clone/fetch only: the embedding and rerank model packages
# always try the Gitee release first and fall back to GitHub, because of how they
# are split across release assets.
# --no-mirror-probe disables all three speed probes (PyPI index, code host and
# Node.js dist mirror); each falls back to its first configured default.
# Re-running the script upgrades in place: when an existing valid configuration is
# detected the wizard is skipped and the configuration is left untouched.
bash install.sh --reconfigure    # force the setup wizard to run again
bash install.sh --skip-setup     # install the code only, never enter the wizard

# For every flag and environment variable, including the model prefetch switches
# (ECHO_SKIP_RERANK_PREFETCH and friends):
bash install.sh --help
```

</details>

### Common commands

```bash
echo-agent run              # Interactive conversation (plain terminal line input)
echo-agent setup            # Setup wizard (models, channels, permissions; safe to rerun)
echo-agent status           # Show current configuration status
echo-agent gateway          # Run the resident gateway in the foreground
echo-agent gateway install  # Register the gateway as a background service (recommended, see below)
echo-agent cli              # Attach to the local resident gateway as a thin client (terminal TUI)
echo-agent cost             # Show cost attribution report
echo-agent dashboard build  # Build the web Dashboard bundle (on demand, for source installs)
```

> Inspect configuration with the CLI: `echo-agent config explain <key>` for a single option (description, type, default and allowed values), `echo-agent config dump` to view the active configuration (secrets are redacted), and `echo-agent config validate` to check a config file.

### Running as a background service

Both `echo-agent run` and `echo-agent gateway` are foreground processes — they exit when the terminal closes. For a 24/7 resident agent, register the gateway as a system service (a user-level LaunchAgent on macOS, a user-level systemd unit on Linux; no root required, auto-start at login, auto-restart on crash):

```bash
echo-agent gateway install    # register the background service
echo-agent gateway start      # start it
echo-agent gateway status     # check whether it is running
echo-agent gateway logs -f    # follow the logs
echo-agent gateway restart    # restart (run once after upgrading echo-agent)
echo-agent gateway stop       # stop it
echo-agent gateway uninstall  # unregister
```

Once the gateway is running, attach from any local terminal with `echo-agent cli` to talk to the same resident agent (separate session, shared memory). The gateway listens on local loopback only (127.0.0.1); remote addresses are not supported — use ssh for remote access.

<details>
<summary>Linux notes / systemd-less environments / legacy command</summary>

- **Keep running after logout** (Linux user services stop with the login session by default): `sudo loginctl enable-linger $USER`
- **Multi-user servers**: `echo-agent gateway install --system` registers a system-wide systemd unit (requires sudo)
- **No systemd** (WSL without systemd, containers, etc.): keep the foreground process alive with tmux or nohup, e.g. `tmux new -s echo-agent 'echo-agent gateway'`
- **Stale service file after an upgrade**: `echo-agent gateway status` warns about it; rewrite with `echo-agent gateway install --force`
- The old `echo-agent service` command still works but is deprecated — use `echo-agent gateway <action>` instead

</details>

> Local security boundary: a zero-config loopback gateway (`allowlist` mode + empty list) serves only two kinds of client — `echo-agent cli` (which carries a `cli:` identity) and native clients that send no browser `Origin` (scripts/SDKs). Browser requests carrying a cross-site `Origin` (including the bundled playground page) are rejected, to stop a malicious web page from driving the local agent via the browser (CSRF). To let a browser/playground in, set `gateway.auth.mode=open`, add the user to `gateway.auth.allowed_users`, or (for a webview desktop client, etc.) add its Origin to `gateway.auth.allowed_origins`.

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
| **Output Preservation** | Oversized tool output is spilled to disk; the model sees a head/tail preview plus a retrieval path and can pull the full text back with `read_spill` by character range or regex |
| **Local-First** | Sessions, memory, traces and credentials stored in the workspace by default, credentials encrypted at rest |

> Starting with this version, tool output exceeding `spill.maxInlineChars` (6000 characters by default)
> is no longer shown to the model in full. It is replaced with "head + tail + spill path". The complete
> content is kept under `storage.spillDir` (`data/spill` by default). If your skills or prompts rely
> on tool output being directly visible and contiguous, set `spill.enabled: false` to turn this off,
> or raise `spill.maxInlineChars`.
>
> **Retrieval goes through `read_spill` only.** Artifacts are per-session private: `read_spill`
> authorises on the calling session's identity and can only retrieve artifacts that session produced
> itself. `read_file` / `search_files` / `list_dir` / `read_document` / `send_file` all refuse the
> artifact directory — they authorise on the path alone, and artifact paths travel in model-visible
> text. `read_spill` reads by **character** range via `offset`/`limit` (so the tail of single-line
> JSON or a compressed log is reachable), or searches within an artifact via `pattern`. Note this is
> a path-layer boundary: with `exec` enabled a shell can still read the file directly, so it only
> constitutes full isolation in deployments where `exec` is off (`minimal` / `messaging` profiles,
> `public_gateway` / `daemon`).
>
> **Reclamation:** artifacts are bounded by both `spill.retentionDays` (default 7) and
> `spill.maxTotalMb` (default 512 MB); whichever triggers first deletes them, so any single artifact
> lives **at most** the retention period and may be reclaimed earlier under the size cap. Sweeps run
> every `spill.sweepIntervalHours` (default 6) and are independent of `spill.enabled` — turning the
> switch off only stops producing new artifacts, existing ones keep being reclaimed.
> `storage.spillDir` must be a workspace-relative subdirectory: the sweeper deletes files under it,
> and only ones matching the `session-*/<hex>-<tool>.txt` shape it wrote itself, never unrelated files.

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
- QQ group: [47572014](https://qm.qq.com/q/JWOPDBNssw)
- [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) — design discussion, usage questions
- [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) — bugs and feature requests

---

## License

[MIT License](LICENSE)
