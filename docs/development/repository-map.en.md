# Repository Map

The server is an independent project under `echo-agent-runtime/`. Downloading that directory includes Python sources, Dashboard sources, bundled skills, build scripts and deployment templates. The desktop app (`client/`), documentation (`docs/`) and repository workflows remain at the root. Table paths below are relative to `echo-agent-runtime/`, except the explicitly marked repository-level entries.

| Path | Responsibility |
|---|---|
| `echo_agent/app.py` | Application composition and lifecycle |
| `echo_agent/__main__.py` | CLI parser and command dispatch |
| `echo_agent/agent/loop.py` | Agent processing loop |
| `echo_agent/agent/pipeline/` | Inference and tool-call stages |
| `echo_agent/agent/tools/` | Built-in tool implementations and registration |
| `echo_agent/agent/executors/` | Local, copied-workdir, container, and remote execution backends |
| `echo_agent/agent/planning/` | Task planning |
| `echo_agent/agent/multi_agent/` | Multi-agent collaboration |
| `echo_agent/tools/` | Public tool extension interface |
| `echo_agent/models/` | Model routing, credential pools, and provider abstraction |
| `echo_agent/models/providers/` | Provider implementations |
| `echo_agent/channels/` | Message channel adapters and manager |
| `echo_agent/memory/` | Memory storage, retrieval, and forgetting |
| `echo_agent/knowledge/` | Document extraction, indexing, and vector storage |
| `echo_agent/gateway/server.py` | aiohttp Gateway and core routes |
| `echo_agent/gateway/api/` | Management API handlers and route registration |
| `echo_agent/gateway/auth.py` | API token and pairing authorization |
| `echo_agent/config/schema.py` | Pydantic configuration models and schema defaults |
| `echo_agent/config/default.yaml` | Packaged configuration overrides |
| `echo_agent/config/loader.py` | Configuration-file, environment, and override merging |
| `echo_agent/config/docgen.py` | Configuration reference generator |
| `echo_agent/storage/sqlite.py` | SQLite connection and automatic schema migrations |
| `echo_agent/checkpoint/` | Shadow Git workspace-file checkpoints |
| `echo_agent/cli/` | Terminal client, background service management, and CLI subcommands |
| `echo_agent/plugins/` | Plugin discovery, manifest admission, and lifecycle |
| `echo_agent/mcp/` | MCP client |
| `echo_agent/observability/` | Internal traces, log buffer, and optional OpenTelemetry |
| `echo_agent/skills/` | Skill loading and runtime management |
| `echo_agent/tasks/` | Task management |
| `echo_agent/artifacts/` | User artifacts |
| `web/src/` | Dashboard React source |
| `skills/` | Bundled domain-organized SKILL.md files and scripts |
| `tests/` | pytest tests |
| repository root `docs/` | MkDocs documentation |
| repository root `scripts/tests/` | Documentation/code consistency checks |
| `deploy/` | Standalone server configuration and container templates |
| `scripts/` | Installation, build, and release scripts |

## Key relationships

`echo_agent/app.py` wires configuration, storage, models, the agent, channels, and the Gateway. `echo_agent/tools/` is the public tool extension interface; `echo_agent/agent/tools/` implements built-in tools. The Gateway uses `aiohttp` and token/pairing authorization, not a JWT server or an ASGI framework. SQLite schema migrations run when `echo_agent/storage/sqlite.py` initializes a connection; `echo-agent migrate` in `echo_agent/cli/migrate_cmd.py` handles memory data.

## Desktop engineering map

| Path, relative to the repository root | Responsibility |
| --- | --- |
| `client/src/` | React UI and application state |
| `client/src-tauri/src/` | Desktop host, ACP bridge, workspaces, automation, persistence |
| `client/vendor/echo-agent-build/` | Pinned Rust execution-engine source |
| `client/vendor/theia-platform/` | Embedded IDE and integration |
| `client/scripts/` | Build, packaging, and validation |

The Python engine under `echo-agent-runtime/` is distinct from the Rust engine embedded by Desktop. See [Architecture](../concepts/architecture.md) for product boundaries.
