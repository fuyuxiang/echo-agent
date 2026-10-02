<div align="center">

# Echo Agent

**An open-source AI Agent that remembers the past and learns for the future**

<a href="https://github.com/fuyuxiang/echo-agent">
  <img src="docs/assets/echo-agent.png" alt="Echo Agent" width="720" />
</a>

<br/>

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![Python](https://img.shields.io/pypi/pyversions/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-latest-blue)](https://fuyuxiang.github.io/echo-agent/en/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Downloads](https://static.pepy.tech/badge/echo-agent)](https://pepy.tech/project/echo-agent)
[![GitHub stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=social)](https://github.com/fuyuxiang/echo-agent)

[中文](README.md) · English · [Documentation](https://fuyuxiang.github.io/echo-agent/en/)

</div>

---

## One kernel, three forms

Echo Agent is an open-source AI Agent ecosystem: a **cognitive-memory + self-evolving** kernel powering three ways to use it.

| Form | What it is | Location |
|------|------------|----------|
| **echo-agent** | A self-hostable Python Agent runtime with long-term memory, skill evolution and 14 channel integrations | repo root |
| **Dashboard** | The gateway's built-in web admin panel for sessions, memory, skills, knowledge and cost | `web/` |
| **EchoAgent** | A desktop Agent client built on the kernel, packaged and shipped independently | `client/` |

All three share one set of Agent capabilities: the desktop client vendors the runtime source under `client/vendor/`, so building it needs no prior Python install; the Dashboard is served at the gateway's root path with no separate port.

---

## Core Highlights

Unlike one-off Q&A, Echo Agent **remembers and learns**:

- **Cognitive memory** — Four tiers (Working / Episodic / Semantic / Archival) with an Ebbinghaus forgetting curve (the more you access, the slower it fades) and contradiction detection (belief revision, never silent overwrite). It solves memory bloat under long-running use — conversations no longer start from scratch.
- **Self-evolving skills** — Improvement candidates are generated from real execution traces, validated against an eval set before promotion, with cooldown and one-click rollback. Skills get better with use instead of being fixed at release.
- **Unified multi-entry** — CLI, Gateway, Webhook, Cron and [14 channels](https://fuyuxiang.github.io/echo-agent/en/integrations/channels/) in total (Telegram / Discord / Slack / WeChat / WeCom / Feishu / DingTalk / QQ / WhatsApp / Email / Matrix) share one set of memory and permissions.
- **Safe and auditable** — High-risk tool calls go through unified approval, credentials are encrypted at rest, execution logs are auditable.

For the remaining capabilities — hybrid retrieval, model routing, MCP / A2A, plugins, scheduled tasks and output preservation — see the [documentation](#documentation) topics below.

---

## Dashboard

The gateway ships with a built-in web admin panel: start the gateway and open its address in a browser, no separate port. It covers the system overview, sessions, memory, skills, knowledge base, scheduled tasks, the task kanban, logs, analytics and runtime configuration.

> Start the gateway with `echo-agent gateway`, then open `http://127.0.0.1:58123/` (the port is set by `gateway.port`); build the frontend bundle with `echo-agent dashboard build`. See [Dashboard](https://fuyuxiang.github.io/echo-agent/en/guides/dashboard/).

---

## EchoAgent Desktop Client

`client/` is a Tauri 2 desktop app: React / TypeScript for the UI, Rust for local capabilities and the in-process Agent runtime. Beyond conversations, the coding workspace, knowledge bases, scheduled automations and controlled Browser Use / Computer Use, its two killer features are:

- **Take over sessions via WeChat** — Once bound by scanning a QR code, you can hand a running task to WeChat, switch tasks, confirm permission requests and send or receive attachments. The desktop runtime processes the commands; desktop and WeChat share one session, and no WeChat command executes while the desktop is closed. See [WeChat remote](client/docs/weixin-remote.md).
- **Organizational knowledge hub** — After signing in to an organization, bring team / company knowledge assets into your personal workspace, retrieved alongside local memory and your personal knowledge index. Access tokens stay in Rust memory and authorization decisions stay on the server; the desktop stores only the HTTPS origin and a refresh credential.

<div align="center">
  <img src="client/docs/images/echoagent-home.png" alt="EchoAgent desktop client" width="820" />
</div>

Local development requires Node.js, pnpm, stable Rust (minimum 1.92) and `protoc`; macOS also needs the Xcode Command Line Tools and Windows needs the "Desktop development with C++" workload from Visual Studio 2022.

```bash
cd client
pnpm install
pnpm tauri dev
```

`pnpm test` runs the frontend tests and `pnpm build` the TypeScript check plus the production build; once platform signing is configured, `pnpm dist` produces an installer (covering Windows x86_64 and both Apple Silicon and Intel macOS). See [release-workflow.md](client/docs/release-workflow.md) for the release flow and [automation-platform-support.md](client/docs/automation-platform-support.md) for desktop automation platform and security boundaries.

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

Behind a slow PyPI mirror, pass an index explicitly: `pip install "echo-agent[all]" -i <index-url>`. On Windows the same three commands work in PowerShell.

<details>
<summary>Source install script (Linux / macOS / WSL2)</summary>

`scripts/install.sh` is a separate path from `pip install`: it clones the source into `~/.echo-agent`, creates a dedicated virtual environment, installs the `[all]` extras, and can register the gateway as a resident service. Use it when you intend to modify the source or want the resident deployment done in one step; for the released package, `pip install` is enough.

The script is downloaded first so its contents can be reviewed before execution:

```bash
# GitHub
curl -fsSL -o install.sh https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh
# Gitee mirror (faster inside mainland China)
curl -fsSL -o install.sh https://gitee.com/fuyuxiang/echo-agent/raw/master/scripts/install.sh

less install.sh && bash install.sh
```

```bash
# The script probes both code hosts and clones from whichever answers faster;
# it can also be pinned explicitly:
bash install.sh --repo github
bash install.sh --repo gitee

bash install.sh --reconfigure    # run the setup wizard again
bash install.sh --skip-setup     # install the code only, without the wizard

# Every flag and environment variable:
bash install.sh --help
```

`--repo` applies to `git clone` / `fetch` only. The embedding and rerank model packages are split across release assets, so they always try the Gitee release first and fall back to GitHub regardless of this flag. `--no-mirror-probe` disables all three speed probes (PyPI index, code host, Node.js dist mirror), leaving each at its first configured default.

Re-running the script upgrades in place: when an existing valid configuration is detected, the wizard is skipped and the configuration is left untouched.

</details>

For the trade-offs between installation methods, the dependency extras beyond `[all]`, and uninstall steps, see the [installation guide](https://fuyuxiang.github.io/echo-agent/en/getting-started/installation/).

### Common commands

```bash
echo-agent run              # Interactive conversation (plain terminal line input)
echo-agent setup            # Setup wizard (models, channels, permissions; safe to rerun)
echo-agent status           # Show current configuration status
echo-agent gateway          # Run the resident gateway in the foreground
echo-agent gateway install  # Register the gateway as a background service (recommended, see below)
echo-agent cli              # Attach to the local resident gateway as a thin client (native scrollback)
echo-agent cli --tui        # Optional full-screen Textual interface
echo-agent cost             # Show cost attribution report
echo-agent dashboard build  # Build the web Dashboard bundle (on demand, for source installs)
```

> Inspect configuration with the CLI: `echo-agent config explain <key>` for a single option (description, type, default and allowed values), `echo-agent config dump` to view the active configuration (secrets are redacted), and `echo-agent config validate` to check a config file.

For every subcommand and flag see the [CLI reference](https://fuyuxiang.github.io/echo-agent/en/reference/cli/); for every configuration option see the [configuration reference](https://fuyuxiang.github.io/echo-agent/en/reference/configuration/).

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

Two environment differences to note: Linux user services stop with the login session, so `sudo loginctl enable-linger $USER` keeps the service running after logout; on hosts without systemd (WSL2 in its default configuration, containers) use tmux to hold the foreground process instead, e.g. `tmux new -s echo-agent 'echo-agent gateway'`. System-wide registration and service-file updates are covered in [background service](https://fuyuxiang.github.io/echo-agent/en/operations/background-service/).

> **Local access boundary**: with no additional configuration the loopback gateway accepts two kinds of client — `echo-agent cli`, and native clients that send no browser `Origin` (scripts, SDKs). Browser requests carrying a cross-site `Origin` are rejected, preventing a web page from driving the local agent through the user's browser (CSRF). See [gateway authentication](https://fuyuxiang.github.io/echo-agent/en/integrations/gateway/authentication/) for opening access to a browser or the playground.

Pairing clients receive distinct verification responses: `400` for invalid request fields, `403` for an invalid or expired code, and `429` with a `Retry-After` wait time when an identity is temporarily locked. See [gateway authentication](https://fuyuxiang.github.io/echo-agent/en/integrations/gateway/authentication/#pairing-failure-lockout) for field limits and lockout behavior.

---

## Documentation

Full documentation lives at **[fuyuxiang.github.io/echo-agent](https://fuyuxiang.github.io/echo-agent/en/)**. This README covers installation and getting started.

| Topic | Entry points |
|-------|--------------|
| Getting started | [Installation](https://fuyuxiang.github.io/echo-agent/en/getting-started/installation/) · [Upgrade & uninstall](https://fuyuxiang.github.io/echo-agent/en/getting-started/upgrade-uninstall/) |
| Core concepts | [Architecture](https://fuyuxiang.github.io/echo-agent/en/concepts/architecture/) · [Memory system](https://fuyuxiang.github.io/echo-agent/en/concepts/memory-system/) · [Skill evolution](https://fuyuxiang.github.io/echo-agent/en/concepts/evolution-evaluation/) · [Security model](https://fuyuxiang.github.io/echo-agent/en/concepts/security-model/) |
| Guides | [Models](https://fuyuxiang.github.io/echo-agent/en/guides/models/) · [Tools & permissions](https://fuyuxiang.github.io/echo-agent/en/guides/tools-permissions/) · [Knowledge base](https://fuyuxiang.github.io/echo-agent/en/guides/knowledge-base/) · [Cost](https://fuyuxiang.github.io/echo-agent/en/guides/cost-control/) |
| Integrations | [Channels](https://fuyuxiang.github.io/echo-agent/en/integrations/channels/) · [Gateway](https://fuyuxiang.github.io/echo-agent/en/integrations/gateway/) · [MCP](https://fuyuxiang.github.io/echo-agent/en/integrations/mcp/) · [A2A](https://fuyuxiang.github.io/echo-agent/en/integrations/a2a/) · [Plugins](https://fuyuxiang.github.io/echo-agent/en/integrations/plugins/) |
| Operations & reference | [Deployment](https://fuyuxiang.github.io/echo-agent/en/operations/) · [CLI](https://fuyuxiang.github.io/echo-agent/en/reference/cli/) · [Configuration](https://fuyuxiang.github.io/echo-agent/en/reference/configuration/) |

---

## Architecture

<div align="center">
  <img src="docs/assets/architecture.png" alt="Echo Agent Architecture" width="820" />
</div>

For component boundaries and data flow see [architecture](https://fuyuxiang.github.io/echo-agent/en/concepts/architecture/); for the repository layout see the [code map](https://fuyuxiang.github.io/echo-agent/en/development/repository-map/).

---

## Use Cases

- Conversations, preferences and task experience that should persist across sessions instead of starting from scratch
- Agent skills that should keep evolving from real usage rather than being fixed at release
- Multiple entry points (CLI, Webhook, chat bots, desktop client) that need to share one set of memory and permissions
- High-risk tools that require mandatory approval to prevent accidental damage

For deployment shapes, capacity planning and the hardening checklist see the [operations docs](https://fuyuxiang.github.io/echo-agent/en/operations/).

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

Currently `0.3.x`, in Beta. Version numbers use the semantic-version format; read each release's notes for actual compatibility:

- **PATCH** (`0.3.x`) is generally used for fixes; read the release notes and back up before upgrading.
- **MINOR** (`0.x.0`) may include breaking configuration or data changes during Beta; read the notes for each release. SQLite schema migrations run automatically during database initialization; `echo-agent migrate` only handles USER memory ownership and legacy memory-shard imports.
- Changes to configuration keys and to the plugin / skill interfaces are itemised in the [CHANGELOG](CHANGELOG.md).

Back up your workspace directory before upgrading. See [upgrade & migrations](https://fuyuxiang.github.io/echo-agent/en/operations/upgrade-migrations/) for the procedure and [compatibility](https://fuyuxiang.github.io/echo-agent/en/reference/compatibility/) for the stability level of each interface.

## Security

Report vulnerabilities through GitHub's [private security advisory](https://github.com/fuyuxiang/echo-agent/security/advisories/new) form; we acknowledge receipt within 48 hours. Disclosure process and supported versions are in [SECURITY.md](SECURITY.md).

For deployment-side boundaries and the hardening checklist see [security model](https://fuyuxiang.github.io/echo-agent/en/concepts/security-model/) and [security hardening](https://fuyuxiang.github.io/echo-agent/en/operations/security-hardening/).

---

## License

[MIT License](LICENSE)
