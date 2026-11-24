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

Sessions, workspace state, memory, and working files are mainly local. Embedding and reranking for personal knowledge and memory run on the device. The selected chat model receives task context, which can include page text, controls, and screenshots from Browser Use or Computer Use. Enabled MCP, connectors, notifications, cloud storage, and organization services access the network according to their own configuration.

Keys are stored in local configuration. Remove credentials and personal or business content before sharing diagnostics. Avoid publishing entire configurations or sessions.

## Back up and restore

Use the backup and recovery options in Settings. Backups include registered sessions, memory, groups, projects, meetings, legacy workspace archives, and selected UI state. **Back up business files in working directories separately.** Preserve model credentials and external-account settings separately as well.

Inspect the restore preview before applying it. Restoration runs before services start and includes interruption recovery. Restoring history does not replay interrupted external operations. Periodically verify backup readability.

## Diagnose by symptom

| Symptom | Check first | Next action |
| --- | --- | --- |
| No model response | Connection test, network, key, model ID | Retry a simple message on a verified connection |
| Text works but tools fail | Tool support, parameters, permissions | Inspect the specific tool error |
| Workspace file will not open | Folder authorization and whether the file is still present | Check the folder and file path in the conversation |
| Browser unavailable | Installed Chromium-family browser | Inspect capability detection |
| Computer task will not start | OS permissions, interactive desktop | Check permissions for the actual running application |
| Document missing | Output path and tool receipt | Inspect the file before retrying |
| Outdated memory | Scope, source, previous facts | Correct or forget it |
| Schedule did not run | Application running, computer awake | Inspect run records and test manually |
| Group work interrupted | Work status and existing output | Inspect before retrying that work |

## Logs and updates

For local diagnostics, check `EchoAgent/logs/echoagent.log*` in the OS local application-data directory. With `ECHO_AGENT_HOME` set, logs are under its `logs/` directory.

Check updates under **Settings → Help and updates**. Save edits, finish or stop tasks, and back up important data first. Desktop and Python Runtime have independent releases.
