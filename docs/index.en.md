# Echo Agent documentation

Echo Agent offers **EchoAgent Desktop (Client)** for interactive work on your computer and **Echo Agent Runtime** for terminal use, messaging integrations, and self-hosted services. This manual covers setup, daily use, architecture, and operations.

## Choose your entry point

| Goal | Entry point | Start here |
| --- | --- | --- |
| Work with files, code, browsers, or desktop applications in a UI | Desktop | [Desktop quickstart](desktop/quickstart.md) |
| Use an Agent in a terminal or manage a service through a browser | Runtime | [Runtime quickstart](getting-started/quickstart.md) |
| Connect messaging platforms, webhooks, or scheduled jobs | Runtime | [Channels](integrations/channels/index.md) and [background services](operations/background-service.md) |
| Understand execution, memory, and tool calls | Both | [Architecture](concepts/architecture.md) |
| Provide Agent services to an application | Runtime Gateway | [Gateway integration](integrations/gateway/index.md) |

## Desktop: complete tasks in a workspace

Desktop brings conversations, plans, tool activity, approvals, file changes, and deliverables into one application. Coding tasks can use the embedded IDE. Office tasks can generate documents. Browser and computer automation depend on device capabilities.

Start by installing the application, choosing a working directory and model, submitting a task, and checking the result.

- [Installation and requirements](desktop/installation.md)
- [Your first task](desktop/quickstart.md)
- [Tasks, sessions, and permissions](desktop/tasks-permissions.md)
- [Coding workspace](desktop/coding.md)
- [Browser and computer automation](desktop/automation.md)
- [Documents, knowledge, and memory](desktop/knowledge-memory.md)
- [Models and extensions](desktop/models-extensions.md)
- [Expert groups and scheduled tasks](desktop/collaboration-scheduling.md)
- [Data management and troubleshooting](desktop/operations.md)

## Runtime: operate your own Agent service

Runtime receives requests, builds context, calls models, executes tools, and persists sessions and memory. Gateway adds Dashboard, HTTP API, and WebSocket access. Channels, knowledge indexing, MCP, and skill evolution are enabled through configuration.

```bash
pip install "echo-agent[all]"
echo-agent setup
echo-agent gateway
```

Open <http://127.0.0.1:58123/> after startup. See the [Runtime quickstart](getting-started/quickstart.md) for installation, verification, and terminal access.

| Goal | Documentation |
| --- | --- |
| Install dependencies and verify the environment | [Installation](getting-started/installation.md) |
| Complete a verifiable task | [First workflow](guides/first-workflow.md) |
| Manage sessions, knowledge, memory, and costs | [Runtime guides](guides/index.md) |
| Operate, upgrade, or recover a service | [Operations](operations/index.md) |
| Look up commands, configuration, and APIs | [Reference](reference/index.md) |

## How the products relate

Both use a cycle of model reasoning, tool execution, and feedback, but the current source uses different runtime implementations. Desktop embeds a Rust execution engine. Standalone Runtime uses a Python engine. Desktop works independently without a Gateway deployment.

Each product manages its model connections, permissions, memory, and plugins. Check page scope before applying instructions; configuration files are not interchangeable. See [Architecture](concepts/architecture.md).

## Reading this manual

**Quickstarts** provide a short working path and success criteria. **Guides** explain tasks. **Concepts** explain design, data flow, and boundaries. **Reference** provides exact parameters. **Development** covers contribution workflows.

Concept pages require no source-code knowledge. Architecture diagrams show boundaries and responsibilities; sequence diagrams show interactions; flowcharts show decisions; data-flow diagrams show sources, processing, and storage.

## Project status

The project is in Beta, with independent releases for each product. Read release notes and back up before upgrading. Runtime requires Python 3.11+. Desktop release targets are macOS and Windows; see the installation pages for architectures and requirements.

The project uses the [MIT license](https://github.com/fuyuxiang/echo-agent/blob/master/LICENSE). Report issues through [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues).
