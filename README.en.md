<div align="center">

<h1><img src="docs/assets/readme/hero.svg" alt="Echo Agent" width="100%" /></h1>

**Echo Agent** is an AI agent with long-term memory, collaboration among specialist agents, and skill evolution.

It supports desktop work, software development, and remote automation.<br />
It can carry project context and lessons from past tasks into future work.

<p>
  <a href="#desktop"><img src="docs/assets/readme/desktop-button.svg" alt="Desktop quickstart" width="164" height="44" /></a>
  &nbsp;
  <a href="#runtime"><img src="docs/assets/readme/runtime-button.svg" alt="Runtime quickstart" width="164" height="44" /></a>
</p>

[![PyPI](https://img.shields.io/pypi/v/echo-agent)](https://pypi.org/project/echo-agent/)
[![CI](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/ci.yml)
[![PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/fuyuxiang/echo-agent?style=flat)](https://github.com/fuyuxiang/echo-agent/stargazers)

<!-- Download badges are refreshed by .github/workflows/download-stats.yml. -->
![Runtime total downloads](.github/badges/runtime-downloads.svg)
![Desktop total downloads](.github/badges/desktop-downloads.svg)
[![Statistics updated](.github/badges/stats-updated.svg)](https://github.com/fuyuxiang/echo-agent/actions/workflows/download-stats.yml)

[Docs](https://fuyuxiang.github.io/echo-agent/en/) · [Architecture](docs/concepts/architecture.en.md) · [中文](README.md)

</div>

## Quickstart

Desktop works with documents, code, and web pages. Runtime runs in a terminal or as a service. Each can run independently.

### Desktop

1. [Download the installer](https://www.ojlab.com/download) for macOS (Apple Silicon or Intel) or Windows x64.
2. Open the app and go to **Settings → Models**. Add and test a model connection, or select a model provided by your organization.
3. Choose a working folder and permission mode, then submit a task. Follow its progress and review the results in the conversation.

[![EchoAgent Desktop home screen with a task composer, model picker, workspace picker, and permission mode](client/docs/images/echoagent-home.png)](client/docs/images/echoagent-home.png)

### Runtime

Run these commands in a virtual environment with Python 3.11 or later. Follow the setup wizard to configure a model and credentials for OpenAI or a compatible service.

If the wizard has already started Gateway, skip the last command.

```bash
python -m pip install "echo-agent[openai]"
echo-agent setup --flow quickstart
echo-agent gateway
```

Once Gateway is running, send a message in the [Playground](http://127.0.0.1:58123/playground) and check the service status in the [Dashboard](http://127.0.0.1:58123/).

For direct terminal use, run `echo-agent run`. Run only one full Runtime instance per workspace.

## Main features

### Memory and skill evolution

New sessions can retrieve relevant summaries from previous sessions, project context, and user preferences.

[Memory system](docs/concepts/memory-system.en.md)

Runtime can use task records to propose skill changes and compare versions against the same set of test cases. Skill evolution is off by default and supports human review and rollback when enabled.

[Skill evolution and evaluation](docs/concepts/evolution-evaluation.en.md)

### Desktop tasks and project collaboration

Desktop can create and preview Word, Excel, PowerPoint, and PDF files, edit code in authorized folders, and operate a browser or supported desktop apps. Plans, tool calls, and file changes appear in the conversation. The selected permission mode governs approvals.

[Code tasks](docs/desktop/coding.en.md) · [Office documents](docs/desktop/knowledge-memory.en.md#generate-and-export-documents) · [Platform support and permissions](docs/desktop/automation.en.md)

Projects retain instructions, reference files, and confirmed decisions for future tasks. Within a project, users can divide work among experts and schedule jobs.

[Collaboration and scheduling](docs/desktop/collaboration-scheduling.en.md) · [Team projects (Chinese)](client/docs/team-projects.md)

### Messaging and extensions

Runtime supports 14 channels, including WeChat, Feishu, DingTalk, Slack, the terminal, webhooks, and scheduled jobs. Gateway provides HTTP and WebSocket APIs for other applications.

[Messaging channels](docs/integrations/channels/index.en.md) · [Gateway API](docs/reference/gateway-api.en.md)

Desktop and Runtime both support MCP. Runtime includes 35 built-in skills and supports plugins. Its current A2A integration handles inbound text tasks delegated by other agents. There is currently no outbound A2A delegation entry point in the agent runtime.

[Runtime extensions](docs/runtime-extensions.en.md) · [A2A integration](docs/integrations/a2a.en.md)

## Documentation

- **Desktop** · [Installation and source builds](docs/desktop/installation.en.md) · [Tasks and permissions](docs/desktop/tasks-permissions.en.md) · [Data management](docs/desktop/operations.en.md)
- **Runtime** · [Installation options](docs/getting-started/installation.en.md) · [Background services](docs/operations/background-service.en.md) · [Security model](docs/concepts/security-model.en.md)
- **Architecture and development** · [System architecture](docs/concepts/architecture.en.md) · [Desktop architecture](docs/concepts/desktop-architecture.en.md) · [Repository map](docs/development/repository-map.en.md)

## Contributing

[Contributing guide](CONTRIBUTING.en.md) · [Report an issue](https://github.com/fuyuxiang/echo-agent/issues) · [QQ community](https://qm.qq.com/q/JWOPDBNssw)

## License

The project's original code uses the PolyForm Noncommercial License 1.0.0. Commercial use outside the license requires separate permission.
