# EchoAgent Agent Runtime

This directory contains the Rust agent runtime embedded in
[EchoAgent Desktop](../README.en.md). It is maintained as source inside the
main repository so desktop releases can build reproducibly from one pinned
tree.

The runtime provides the execution engine behind EchoAgent sessions:

- multi-turn agent orchestration, context compaction, and session persistence;
- tool discovery and execution for files, terminals, search, MCP, and skills;
- permission gates, sandbox integration, hooks, and plugin loading;
- subagents, worktrees, checkpoints, background tasks, and status events;
- ACP, headless, terminal, and workspace service adapters;
- provider-neutral sampling used by EchoAgent Desktop's BYOK configuration.

EchoAgent Desktop supplies provider endpoints and credentials at runtime. A
hosted EchoAgent account is not required for the desktop integration.

## Integration contract

| Surface | EchoAgent value |
| --- | --- |
| Runtime source root | `agent/` |
| Crate/package prefix | `echo-agent-` |
| Rust module prefix | `echo_agent_` |
| Protocol namespace | `echo.agent` |
| Runtime home variable | `ECHO_AGENT_HOME` |
| Default runtime home | `~/.echo-agent` |
| Bundled ripgrep variable | `ECHO_AGENT_TOOLS_BUNDLE_RG_PATH` (optional local override) |

Legacy home and theme values are read only by narrowly scoped compatibility
paths so existing local data can be imported safely.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `crates/echo-agent-runtime` | Session lifecycle, sampling, persistence, auth adapters, and runtime APIs |
| `crates/echo-agent-core` | Agent composition, prompts, plugins, and tool wiring |
| `crates/echo-agent-tools` | Built-in tool implementations and schemas |
| `crates/echo-agent-workspace` | Filesystem, process, VCS, worktree, and checkpoint services |
| `crates/echo-agent-mcp` | MCP client and server integration |
| `terminal/echo-agent-pager*` | Terminal UI, rendering, and PTY validation harnesses |
| `crates/echo-agent-tool-*` | Shared protocol, schemas, and tool execution |
| `crates/echo-agent-proto-build` | Shared protobuf build helper |
| `crates/echo-agent-chat-proxy-types` | Shared chat transport types |
| `../third-party/rust/agent-rendering` | Third-party rendering crates included in this Cargo workspace |

Keep package-level changes in the relevant crate manifest and update the root
manifest only when workspace membership or shared dependencies change.

## Validation

Run these commands from the EchoAgent Desktop repository root:

```sh
node scripts/verify-agent-source.mjs

ECHO_AGENT_TOOLS_BUNDLE_RG_PATH=/usr/local/bin/rg \
cargo check --locked \
  --manifest-path agent/Cargo.toml \
  --workspace --all-targets

cargo clippy --locked \
  --manifest-path src-tauri/Cargo.toml \
  --lib -- -D warnings
```

The override is optional. Release builds automatically download the pinned
ripgrep asset for macOS, Linux, or Windows, verify its SHA-256 digest, and embed
the executable in `echo-agent-tools`. Set the path only for offline builds or
to validate a local binary explicitly.

## Provenance and licensing

The snapshot's upstream revision and integration metadata are recorded in
[`SOURCE.json`](SOURCE.json). EchoAgent-specific maintenance
notes live in [`MAINTENANCE.md`](MAINTENANCE.md).

The upstream source remains available under the Apache License 2.0. See
[`LICENSE`](LICENSE), [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES), and
[`agent-rendering/NOTICE`](../third-party/rust/agent-rendering/NOTICE) for attribution and bundled component
licenses.

Security reports should follow [`SECURITY.md`](SECURITY.md).
