<div align="center">

# Echo Agent

**An open-source AI agent with long-term memory, continuous learning, and real task execution**

Runs on your own computer or server: either resident 24/7 behind WeChat, Telegram, Slack and other chat channels, or driving files, browsers and local apps directly from the desktop workspace.

**Self-hosted · Persistent Memory · Desktop Agent · Multi-channel · Human-in-the-loop**

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Python](https://img.shields.io/pypi/pyversions/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-latest-blue)](https://fuyuxiang.github.io/echo-agent/en/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)
[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)

[Quick start](#quick-start) · [Echo Agent Desktop](#echo-agent-desktop-desktop-workspace) · [中文](README.md) · English · [Documentation](https://fuyuxiang.github.io/echo-agent/en/)

</div>

---

## What is Echo Agent

One repository, two forms, sharing the same kernel capabilities (cognitive memory · self-evolving skills · tool approval · model routing):

```text
                       Echo Agent
         ┌──────────────┴──────────────┐
    Agent Runtime                 Echo Agent Desktop
    (Python package, pip install)  (Tauri desktop app, same runtime embedded)
    Always-on assistant            The workspace that works on your computer
    ├── 14 channels: WeChat /      ├── Coding workspace
    │   Telegram / …               ├── Browser Use / Computer Use
    ├── CLI / Webhook / Cron       ├── Files · Knowledge base · Automations
    └── Dashboard web panel        └── WeChat remote takeover
```

### Agent Runtime (always-on assistant)

`pip install` it onto your own computer or server, start a resident gateway with `echo-agent gateway`: 14 channels — WeChat, Telegram, DingTalk, Feishu and more — share one set of memory and permissions, alongside CLI, Webhook and cron jobs. **It is wherever you are; entries can switch, the long-term memory never starts over.**

### Echo Agent Desktop (desktop workspace)

A Tauri 2 desktop app with the Agent runtime embedded (vendored under `client/vendor/`, no Python install needed first). **Not just chatting with AI — hand it a task and it gets done on your computer**: local files and projects, controlled browser / computer operation, a parallel task board, knowledge bases and scheduled automations. When you are away, continue the task and approve permission requests from WeChat. Every high-risk action asks for your confirmation before it runs.

<div align="center">
  <img src="client/docs/images/echoagent-home.png" alt="Echo Agent Desktop" width="820" />
</div>

> The Dashboard is the gateway's built-in web admin panel: start the gateway and open it in a browser (default `http://127.0.0.1:58123/`, the port set by `gateway.port`) — system overview, sessions, memory, skills, knowledge, tasks and logs in one place. See the [Dashboard guide](https://fuyuxiang.github.io/echo-agent/en/guides/dashboard/).

---

## What it can do

Four real task shapes (all capabilities are implemented; boundaries noted per section):

- 💻 **"Analyze this project and fix the failing tests"** — Desktop coding workspace: read the project → locate → edit code → run tests → show the result, with tasks and permissions visible in one panel.
- 📊 **"Every weekday morning, turn yesterday's data into a daily report and send it to Feishu"** — Runtime cron jobs: the agent runs on schedule and the result is delivered as a message to the channel you choose.
- 🌐 **"Research 10 competitors and organize them into a table"** — Desktop Browser Use: a controlled browser visits each site, extracts information and assembles the output; each site action goes through your confirmation first.
- 📱 **"I'm heading out — keep that task running"** — WeChat takeover: the desktop task hands over to WeChat; keep talking, watch progress and approve permission requests on the road. Nothing executes over WeChat while the desktop app is closed.

---

## Core capabilities

- **Remembers you** — Conversations, preferences and task experience persist across sessions and channels. Low-value memories decay automatically over long-running use, and contradictory information gets revised instead of silently overwriting.
- **Gets better with use** — Reusable skills are distilled from real execution traces: candidate improvements are validated against an eval set before promotion, with cooldown and one-click rollback. Skills evolve with usage instead of being fixed at release.
- **Actually executes** — On the desktop, it operates files, browsers and local apps to finish tasks; every browser / computer click, keystroke and upload is individually confirmed beforehand, with the confirm card defaulting to decline.
- **Your data stays yours** — Fully self-hosted: data lives on your own machine, credentials are encrypted at rest, with no dependency on any cloud service.
- **Safe and auditable** — High-risk tool calls go through unified approval, outbound requests pass a shared SSRF guard, and execution logs are auditable.

For the remaining capabilities — hybrid retrieval, model routing, MCP / A2A (inbound task endpoint; the Agent runtime has no outbound A2A delegation entry point), plugins and output preservation — see the [documentation](#documentation); the internals of memory and skills are covered in [memory system](https://fuyuxiang.github.io/echo-agent/en/concepts/memory-system/) and [skill evolution](https://fuyuxiang.github.io/echo-agent/en/concepts/evolution-evaluation/).

---

## Quick Start

### Agent Runtime

Requirements: Python 3.11+, at least one model API key.

```bash
# Install
pip install "echo-agent[all]"

# Interactive setup wizard (prompts for your model API key; data lives in ~/.echo-agent by default)
echo-agent setup

# Run an interactive conversation
echo-agent run
```

Behind a slow PyPI mirror, pass an index explicitly: `pip install "echo-agent[all]" -i <index-url>`. On Windows the same three commands work in PowerShell.

<details>
<summary>Source install script (Linux / macOS / WSL2)</summary>

`scripts/install.sh` clones the source from Git, creates a dedicated virtual environment and installs the dependencies, optionally registering a resident service in one step; re-running it upgrades in place. The script is downloaded first so its contents can be reviewed before execution:

```bash
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
# Gitee mirror (faster inside mainland China)
curl -fsSL -o install.sh https://gitee.com/fuyuxiang/echo-agent/raw/master/scripts/install.sh

less install.sh && bash install.sh          # full options: bash install.sh --help
```

</details>

For the trade-offs between installation methods, dependency extras and uninstall steps see the [installation guide](https://fuyuxiang.github.io/echo-agent/en/getting-started/installation/); for every subcommand see the [CLI reference](https://fuyuxiang.github.io/echo-agent/en/reference/cli/), and for every option the [configuration reference](https://fuyuxiang.github.io/echo-agent/en/reference/configuration/).

### Running 24/7 as a background service

`echo-agent run` is a foreground process; to keep the agent resident 24/7, register the gateway as a system service (user-level LaunchAgent on macOS, user-level systemd unit on Linux, no root required):

```bash
echo-agent gateway install    # register and start
echo-agent gateway logs -f    # follow the logs
echo-agent gateway status     # check whether it is running
```

The gateway listens on local loopback only (127.0.0.1), cross-site browser requests are rejected by default, and remote access goes over ssh. For staying alive after logout on Linux, hosts without systemd and more, see [background service](https://fuyuxiang.github.io/echo-agent/en/operations/background-service/) and [gateway authentication](https://fuyuxiang.github.io/echo-agent/en/integrations/gateway/authentication/).

### Building Echo Agent Desktop

The installer distribution channel is under construction; for now build from source (Node.js, pnpm, Rust 1.92+ and `protoc`; platform prerequisites in the [client docs](client/docs/release-workflow.md)):

```bash
cd client
pnpm install
pnpm tauri dev
```

`pnpm test` runs the frontend tests, `pnpm build` the TypeScript check plus the production build, and `pnpm dist` the platform installers. See [release-workflow.md](client/docs/release-workflow.md) for the release flow and [automation-platform-support.md](client/docs/automation-platform-support.md) for desktop automation boundaries.

---

## 14 channels

A single Agent instance can serve multiple channels at once, all sharing one set of memory and permissions:

| | Channels |
|------|------|
| Global IM | Telegram · Discord · Slack · Matrix · WhatsApp · Email |
| Chinese IM | WeChat · WeCom · DingTalk · Feishu / Lark · QQ bot |
| Other entries | CLI · Webhook · Cron |

Telegram, Discord and Slack support message editing and reactions; WeChat and the QQ bot (depending on configuration) can send files. For the full capability matrix per channel and how to connect each one, see the [channels documentation](https://fuyuxiang.github.io/echo-agent/en/integrations/channels/).

---

## Architecture

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent Architecture" width="820" />
</div>

For component boundaries and data flow see [architecture](https://fuyuxiang.github.io/echo-agent/en/concepts/architecture/); for the repository layout see the [code map](https://fuyuxiang.github.io/echo-agent/en/development/repository-map/).

---

## Documentation

Full documentation lives at **[fuyuxiang.github.io/echo-agent](https://fuyuxiang.github.io/echo-agent/en/)**. This README covers installation and getting started.

| Topic | Entry points |
|-------|--------------|
| Getting started | [Installation](https://fuyuxiang.github.io/echo-agent/en/getting-started/installation/) · [Upgrade & uninstall](https://fuyuxiang.github.io/echo-agent/en/getting-started/upgrade-uninstall/) |
| Core concepts | [Architecture](https://fuyuxiang.github.io/echo-agent/en/concepts/architecture/) · [Memory system](https://fuyuxiang.github.io/echo-agent/en/concepts/memory-system/) · [Skill evolution](https://fuyuxiang.github.io/echo-agent/en/concepts/evolution-evaluation/) · [Security model](https://fuyuxiang.github.io/echo-agent/en/concepts/security-model/) |
| Guides | [Models](https://fuyuxiang.github.io/echo-agent/en/guides/models/) · [Tools & permissions](https://fuyuxiang.github.io/echo-agent/en/guides/tools-permissions/) · [Knowledge base](https://fuyuxiang.github.io/echo-agent/en/guides/knowledge-base/) · [Cost](https://fuyuxiang.github.io/echo-agent/en/guides/cost-control/) |
| Integrations | [Channels](https://fuyuxiang.github.io/echo-agent/en/integrations/channels/) · [Gateway](https://fuyuxiang.github.io/echo-agent/en/integrations/gateway/) · [MCP](https://fuyuxiang.github.io/echo-agent/en/integrations/mcp/) · [A2A](https://fuyuxiang.github.io/echo-agent/en/integrations/a2a/) · [Plugins](https://fuyuxiang.github.io/echo-agent/en/integrations/plugins/using-plugins/) |
| Operations & reference | [Deployment](https://fuyuxiang.github.io/echo-agent/en/operations/) · [CLI](https://fuyuxiang.github.io/echo-agent/en/reference/cli/) · [Configuration](https://fuyuxiang.github.io/echo-agent/en/reference/configuration/) |

---

## Development & Contributing

Set up a development environment from source:

```bash
git clone https://github.com/fuyuxiang/echo-agent.git   # mirror: https://gitee.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all,dev]"
```

Run the same checks CI runs before submitting:

```bash
ruff check .
pytest
```

### Submitting a PR

- Branch off `master`; keep one PR to one topic.
- For user-facing changes, update both `README.md` and `README.en.md`; for documentation changes, update both language versions.
- After changing configuration fields, run `echo-agent config gen-docs` to regenerate the configuration reference.
- Work through the checklist in the PR template. CI runs six checks: lint, tests, security scan, Dashboard build, docs build and packaging.

See [CONTRIBUTING](CONTRIBUTING.en.md) for the full conventions and the [development guide](https://fuyuxiang.github.io/echo-agent/en/development/setup/) for environment and debugging details.

### Where to contribute

| Area | Entry point |
|------|-------------|
| Channel adapters | [Adding a channel](https://fuyuxiang.github.io/echo-agent/en/development/add-channel/) |
| Built-in tools | [Adding a tool](https://fuyuxiang.github.io/echo-agent/en/development/add-tool/) |
| Model providers | [Adding a provider](https://fuyuxiang.github.io/echo-agent/en/development/add-provider/) |
| Skills and plugins | [Skill authoring](https://fuyuxiang.github.io/echo-agent/en/development/skill-authoring/) · [Plugin API](https://fuyuxiang.github.io/echo-agent/en/development/plugin-api/) |
| Eval datasets | [Testing and evaluation](https://fuyuxiang.github.io/echo-agent/en/development/testing-evaluation/) |
| Documentation | [Documentation guide](https://fuyuxiang.github.io/echo-agent/en/development/documentation/) |

### Getting in touch

| Channel | Use it for |
|---------|-----------|
| [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) | Bug reports and feature proposals; Bug / Feature templates provided |
| [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) | Usage questions, design discussion, sharing setups |
| QQ group [47572014](https://qm.qq.com/q/JWOPDBNssw) | Real-time chat (Chinese) |

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

---

## Versioning & Compatibility

Currently `0.3.x`, in Beta. Version numbers use the semantic-version format; read each release's notes for actual compatibility.

- **PATCH** (`0.3.x`) is generally used for fixes; read the release notes and back up before upgrading.
- **MINOR** (`0.x.0`) may include breaking configuration or data changes during Beta; read the notes for each release.
- Changes to configuration keys and to the plugin / skill interfaces are itemised in the [CHANGELOG](CHANGELOG.md).

Back up your workspace directory before upgrading. See [upgrade & migrations](https://fuyuxiang.github.io/echo-agent/en/operations/upgrade-migrations/) for the procedure and [compatibility](https://fuyuxiang.github.io/echo-agent/en/reference/compatibility/) for the stability level of each interface.

## Security

Report vulnerabilities through GitHub's [private security advisory](https://github.com/fuyuxiang/echo-agent/security/advisories/new) form; we acknowledge receipt within 48 hours. Disclosure process and supported versions are in [SECURITY.md](SECURITY.md).

For deployment-side boundaries and the hardening checklist see [security model](https://fuyuxiang.github.io/echo-agent/en/concepts/security-model/) and [security hardening](https://fuyuxiang.github.io/echo-agent/en/operations/security-hardening/).

---

## License

[MIT License](LICENSE)
