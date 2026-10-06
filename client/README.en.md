<p align="center"><img src="app-icon.png" width="88" height="88" alt="EchoAgent icon" /></p>

<h1 align="center">EchoAgent</h1>

<p align="center"><strong>Let AI finish work in a real workspace, with every step visible and under your control.</strong><br />An open-source, local-first desktop agent workspace for conversations, tool use, coding, and reviewable delivery.</p>

<p align="center"><a href="README.md">简体中文</a> · <a href="#quick-start">Quick start</a> · <a href="#capabilities">Capabilities</a> · <a href="#architecture-and-data-flow">Architecture</a> · <a href="#contributing">Contribute</a> · <a href="https://github.com/fuyuxiang/echo-agent-desktop/issues">Issues</a></p>

<p align="center"><a href="https://github.com/fuyuxiang/echo-agent-desktop/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/fuyuxiang/echo-agent-desktop/ci.yml?branch=main&amp;label=CI" alt="CI status" /></a> <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-2ea44f" alt="MIT license" /></a> <img src="https://img.shields.io/badge/desktop-macOS%20%7C%20Windows-2563eb" alt="Desktop platforms: macOS and Windows" /> <img src="https://img.shields.io/badge/Tauri-2-24c8db" alt="Tauri 2" /></p>

<p align="center"><img src="docs/images/echoagent-home.png" width="100%" alt="EchoAgent home screen with a task composer, model picker, workspace picker, and permission mode" /></p>

EchoAgent binds each conversation to a real working directory. Choose a model and permission mode, describe a goal, then inspect plans, tool calls, file changes, and results in one desktop app. Most application state stays on your computer; model inference and optional external services still require network access.

## Capabilities

| Area | Current implementation |
| --- | --- |
| **Agent tasks** | Streaming conversations, plans and tasks, tool calls, approvals, session history, rewind and fork, and subagent status. |
| **Coding** | Embedded Eclipse Theia IDE with an editor, file tree, search, terminal, Git, and previews; Echo Code manages task stages, diff review, verification, and delivery. |
| **Browser and desktop control** | Browser Use has a separate profile for each task. Computer Use operates from screenshots and coordinate frames. Availability is detected on the current device; sensitive interactions require confirmation. |
| **Models and extensions** | Built-in remote models and custom model connections; MCP, Skills, Plugins, connectors, and reusable experts. |
| **Knowledge and documents** | Local knowledge index, project context, memory, common-file reading, and local generation of Word, PDF, Excel, and PowerPoint files. |
| **Ongoing work** | Projects and work items, scheduled automations, notifications, meeting recordings and minutes, plus optional WeChat remote sessions and organization services. |

Read more: [Echo Code](docs/echo-code-theia.md) · [Browser Use / Computer Use](docs/automation-platform-support.md) · [Office documents](docs/office-documents.md) · [Live information](docs/live-information.md) · [WeChat remote sessions](docs/weixin-remote.md) · [Skill capability declarations](docs/skill-capability-manifest.md)

## Quick start

### Install the desktop app

Check [GitHub Releases](https://github.com/fuyuxiang/echo-agent-desktop/releases) for an installer matching your operating system and processor. Desktop release targets are **Windows x86_64**, **macOS Apple Silicon**, and **macOS Intel**. If no suitable installer is available, run from source as described below. There is currently no official Linux desktop package.

### Run from source

You need Node.js **22 or 24**, pnpm **10**, Rust **1.92.0+** (stable), the Protocol Buffers compiler `protoc`, and your platform's native build tools. macOS requires Xcode Command Line Tools. Windows requires the Visual Studio 2022 C++ build tools and Windows SDK. The embedded Agent Runtime source is committed in this repository; no submodule initialization is needed.

**macOS**

```bash
git clone https://github.com/fuyuxiang/echo-agent-desktop.git
cd echo-agent-desktop
pnpm setup:mac
pnpm install --frozen-lockfile
pnpm tauri dev
```

**Windows (PowerShell)**

```powershell
git clone https://github.com/fuyuxiang/echo-agent-desktop.git
cd echo-agent-desktop
pnpm setup:win
pnpm install --frozen-lockfile
.\dev.bat
```

The first launch builds and stages the embedded Theia IDE, Office Worker, and Rust Runtime, so it can take longer than later builds. `pnpm tauri dev` starts the full desktop app; `pnpm dev` starts only the Vite frontend.

### Start your first task

1. Select a working directory, model, and permission mode on the home screen.
2. Describe your goal and send it. For file tasks, select the directory you intend to authorize.
3. Follow the plan and tool activity, handle approval requests when they appear, and inspect diffs and verification results for file changes.

Built-in models **do not require your own API key, but inference uses a project-configured remote service**. To use your own model, add and test a connection and model under **Settings → Models**. Custom connections support OpenAI, Anthropic, and compatible APIs. Tool use and other model features depend on the service. Personal model configuration is stored locally in `~/.echo-agent/config.toml`.

<p align="center"><img src="docs/images/echoagent-task-flow.png" width="100%" alt="Typical task flow: choose a workspace, select a model and mode, send a goal, plan and use tools, review permissions, inspect changes, verify and deliver; inspection may lead to another iteration" /></p>

<p align="center"><sub>This shows a typical file-editing task. Skip approval or change-review stages when they do not apply.</sub></p>

## Architecture and data flow

React 18 renders the interface. Tauri 2 and Rust manage native capabilities, state, and processes. The Agent Runtime runs **in process** and communicates with the host over typed ACP channels. The host starts the Echo Code Theia backend as a local process. The Runtime accesses browser and computer automation through a local, task-bound MCP service.

<p align="center"><img src="docs/images/echoagent-architecture.png" width="100%" alt="EchoAgent data flow: the user interacts with React; Tauri commands and events connect the UI to the Rust host; ACP channels connect the host to the Agent Runtime; the host launches Theia; the Runtime connects to models, MCP, Skills, and task-bound automation; the host and Runtime access local workspace data" /></p>

| Directory | Responsibility |
| --- | --- |
| [`src/`](src/) | React UI, state management, and frontend domain logic. |
| [`src-tauri/src/`](src-tauri/src/) | Tauri commands, ACP bridge, policy, storage, coding workbench, and automation backend. |
| [`vendor/echo-agent-build/`](vendor/echo-agent-build/) | Pinned embedded Agent Runtime source snapshot. |
| [`vendor/theia-platform/`](vendor/theia-platform/) | Eclipse Theia source and IDE integration. |
| [`scripts/`](scripts/) · [`docs/`](docs/) | Build, verification, release scripts, and detailed documentation. |

### Data and permission boundaries

- The default data root is `~/.echo-agent/`. Set `ECHO_AGENT_HOME` before launch to change it. Model configuration lives in `config.toml`, MCP configuration in `mcp.json`, and sessions and workbench state in subdirectories under that root.
- Provider API keys are stored in a local configuration file. Protect it and do not commit it or attach it to a public issue. Personal model connections require HTTPS by default; the built-in models use the project's configured remote HTTP service.
- “Local-first” does not mean offline. Task content sent to a model, as well as webpage content or screenshots provided through Browser Use or Computer Use, reaches the selected model service. MCP, notifications, cloud storage, and optional organization features may also access external services.
- Workspace authorization, permission modes, and action confirmations govern Agent activity. Browser Use restricts private-network access by default. Computer Use clicks, drags, typing, and key presses require per-action confirmation even in Always Allow mode. Theia's interactive terminal runs as the current OS user; review commands from unfamiliar projects.

See the [automation platform and security guide](docs/automation-platform-support.md) for platform requirements and operation boundaries.

## Development

| Command | Purpose |
| --- | --- |
| `pnpm tauri dev` | Start the full desktop app, preparing Theia and the Office Worker automatically. |
| `pnpm test` | Run frontend Vitest tests. |
| `pnpm build` | Type-check TypeScript and build the production frontend. |
| `pnpm office:test` | Verify the built-in document generator. |
| `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib --tests -j 2` | Run Rust unit and integration tests. |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --check` | Check Rust formatting. |
| `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --lib -- -D warnings` | Run Rust static analysis. |
| `pnpm dist:mac` / `pnpm dist:win` | Build an installer on the corresponding platform; production releases require platform signing. |

See the [desktop release guide](docs/desktop-release.en.md) for packaging.

## Contributing

Contributions to issues, docs, tests, and code are welcome. Create a branch from `main`, run checks relevant to your change, and describe the change, risks, and verification in your pull request. Open an [issue](https://github.com/fuyuxiang/echo-agent-desktop/issues) to discuss larger features or architectural changes.

## FAQ

<details>
<summary>Can I use a local model?</summary>

Yes. Add a local service with an OpenAI- or Anthropic-compatible API as a custom connection. HTTP connections must be explicitly allowed under the app's connection policy. Tool use and multimodal support depend on the specific model.

</details>

<details>
<summary>Do I need an EchoAgent account for personal use?</summary>

No. Use a built-in model or configure your own model connection. Organization services are optional.

</details>

<details>
<summary>Is Linux supported?</summary>

The automation backend has some Linux platform support, but there is currently no official Linux desktop installer. See the [automation platform guide](docs/automation-platform-support.md) for its conditions.

</details>

## License and acknowledgements

EchoAgent application code is licensed under the [MIT License](LICENSE). The embedded Runtime, Theia, and other third-party components retain their own licenses; see [Third-Party Notices](THIRD_PARTY_NOTICES.md). Built with [Tauri](https://tauri.app/), [React](https://react.dev/), and [Eclipse Theia](https://theia-ide.org/).
