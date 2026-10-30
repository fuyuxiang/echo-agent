# Data management and troubleshooting

**Scope: Desktop.** This page lists operational data locations. Concept pages explain the design without source-code paths.

## Configuration and storage

| Content | Default location or entry | Notes |
| --- | --- | --- |
| Data root | `~/.echo-agent/` | Override with `ECHO_AGENT_HOME` before launch |
| Model connections | `config.toml` | Protect API keys in this Desktop configuration |
| MCP settings | `mcp.json` | Connection and tool configuration |
| Expert groups | `collaboration.sqlite` | Persistent chat, work, and decisions |
| Sessions and memory | Corresponding directories under the root | No automatic Python Runtime synchronization |
| Working files | The task's chosen directory | Back up separately |

Both products can use the same default root, but have different formats and storage structures. Define each data directory and backup scope when testing or using both.

## Local storage and remote processing

Sessions, workspace state, memory, and working files are mainly local. Models receive selected context. Automation can provide page text, controls, and screenshots. Desktop configures remote memory embedding and reranking services, which may receive retrieval text. Enabled MCP, connectors, notifications, cloud storage, and organization services have their own network interactions.

Keys are stored in local configuration. Remove credentials and personal or business content before sharing diagnostics. Avoid publishing entire configurations or sessions.

## Back up and restore

Use **Settings → Data management**. Backups include registered sessions, memory, groups, projects, meetings, coding state, and selected UI state. **Back up business files in working directories separately.** Preserve model credentials and external-account settings separately as well.

Inspect the restore preview before applying it. Restoration runs before services start and includes interruption recovery. Restoring history does not replay interrupted external operations. Periodically verify backup readability.

## Diagnose by symptom

| Symptom | Check first | Next action |
| --- | --- | --- |
| No model response | Connection test, network, key, model ID | Retry a simple message on a verified connection |
| Text works but tools fail | Tool support, parameters, permissions | Inspect the specific tool error |
| IDE will not open | Workspace authorization, backend, project dependencies | Preserve unsaved work before reconnecting |
| Browser unavailable | Installed Chromium-family browser | Inspect capability detection |
| Computer task will not start | OS permissions, interactive desktop | Check permissions for the actual running application |
| Document missing | Output path and tool receipt | Inspect the file before retrying |
| Outdated memory | Scope, source, previous facts | Correct or forget it |
| Schedule did not run | Application running, computer awake | Inspect run records and test manually |
| Group work interrupted | Work status and existing output | Inspect before retrying that work |

## Logs and updates

For IDE failures, inspect `theia.log`. Defaults use the OS application-data directory; a custom `ECHO_AGENT_HOME` stores IDE configuration and logs under `theia/`.

Check updates under **Settings → Help and updates**. Save edits, finish or stop tasks, and back up important data first. Desktop and Python Runtime have independent releases.
