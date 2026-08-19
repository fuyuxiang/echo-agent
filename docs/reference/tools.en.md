# Built-in Tools Reference

Echo Agent includes 30 built-in tools organized by risk category. Tool availability depends on the active tool profile and per-tool approval settings.

---

## Tool Profiles

| Profile | Includes | Use Case |
|---------|----------|----------|
| `minimal` | MINIMAL_TOOLS only | Read-only assistant, safe for untrusted contexts |
| `messaging` | MINIMAL + MESSAGING_TOOLS | Personal assistant with media capabilities |
| `coding` | MINIMAL + MESSAGING + CODING_TOOLS | Development assistant with file/code access |
| `full` | All tools | Full autonomy including high-risk operations |

Configure via:

```yaml
tools:
  profile: messaging  # minimal | messaging | coding | full
```

---

## Approval Modes

Each tool has an approval mode that determines whether user confirmation is needed:

| Mode | Behavior |
|------|----------|
| `auto` | Execute immediately without asking |
| `ask` | Prompt user for approval before executing |
| `deny` | Tool is disabled, cannot be invoked |

Override per-tool approval in config:

```yaml
tools:
  profile: coding
  overrides:
    shell:
      approval: ask
    process:
      approval: deny
    filesystem:
      approval: auto
```

---

## Risk Categories

### MINIMAL_TOOLS (Read-Only)

Safe, read-only tools that cannot modify system state.

| Tool | Description | Typical Use |
|------|-------------|-------------|
| `browser` | Browse and extract web page content | Research, reading documentation |
| `clarify` | Ask the user a clarification question | Resolving ambiguity before acting |
| `knowledge` | Query the knowledge base | RAG retrieval from indexed documents |
| `memory` | Read/write persistent memory | Recalling user preferences, facts |
| `read_spill` | Read spilled (overflow) content | Accessing large prior outputs |
| `search` | Web search via configured provider | Finding current information |
| `session_search` | Search conversation history | Finding past discussions |
| `skills` | List available skills | Discovering capabilities |
| `todo` | Manage task/todo lists | Tracking work items |
| `vision` | Analyze images | Understanding screenshots, diagrams |
| `web` | Fetch URL content | Reading APIs, web pages |

### MESSAGING_TOOLS (+ Media)

Tools that send messages or generate media content.

| Tool | Description | Typical Use |
|------|-------------|-------------|
| `message` | Send message to a channel | Replying on Telegram, Discord, etc. |
| `notify` | Send push notification | Alerting user of completed tasks |
| `send_file` | Send file to user | Delivering generated documents |
| `image_gen` | Generate images (default provider) | Creating diagrams, illustrations |
| `image_gen_fal` | Generate images via Fal.ai | Alternative image generation |
| `tts` | Text-to-speech synthesis | Audio message generation |

### CODING_TOOLS (+ Write Access)

Tools that can modify files, execute code, or manage workflows.

| Tool | Description | Typical Use |
|------|-------------|-------------|
| `code_exec` | Execute code in sandboxed environment | Running Python/JS snippets |
| `filesystem` | Read/write/delete files | File management operations |
| `patch` | Apply unified diffs to files | Code modifications |
| `shell` | Execute shell commands | System operations, git, builds |
| `document` | Generate structured documents | Creating reports, specs |
| `delegate` | Spawn a sub-agent for a task | Parallel work decomposition |
| `task` | Add items to the task queue | Deferred execution |
| `workflow` | Execute multi-step workflows | Complex orchestrated operations |
| `skill_run` | Execute an installed skill | Running evolved capabilities |

### HIGH_RISK_TOOLS (Execution + System)

Tools with significant system impact. Require `full` profile.

| Tool | Description | Typical Use |
|------|-------------|-------------|
| `process` | Manage system processes (start/stop/signal) | Service management |
| `cronjob` | Create/modify scheduled jobs | Automated recurring tasks |
| `skill_install` | Install new skills from external sources | Extending agent capabilities |

!!! danger "High-risk tool safety"
    HIGH_RISK_TOOLS can make persistent system changes. Even with `full` profile, these default to `ask` approval mode. Set to `auto` only in fully trusted environments.

---

## Tool Details

### browser

Browse web pages with full rendering, extract structured content.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | str | yes | URL to browse |
| `selector` | str | no | CSS selector to extract specific content |
| `wait_for` | str | no | Wait for element before extraction |
| `screenshot` | bool | no | Capture page screenshot |

```yaml
# Example invocation
tool: browser
args:
  url: "https://docs.example.com/api"
  selector: ".main-content"
```

### clarify

Request clarification from the user before proceeding.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `question` | str | yes | Question to ask the user |
| `options` | list | no | Suggested answer options |
| `context` | str | no | Why this clarification is needed |

### code_exec

Execute code in a sandboxed environment with resource limits.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `language` | str | yes | Language: `python`, `javascript`, `bash` |
| `code` | str | yes | Code to execute |
| `timeout` | int | no | Max execution time (seconds, default: 30) |
| `memory_limit_mb` | int | no | Memory cap (default: 256) |

```yaml
tool: code_exec
args:
  language: python
  code: |
    import json
    data = {"result": sum(range(100))}
    print(json.dumps(data))
  timeout: 10
```

!!! tip "Sandbox isolation"
    `code_exec` runs in an isolated environment with no network access and limited filesystem visibility. Use `shell` for operations requiring full system access.

### cronjob

Create, list, or remove scheduled recurring tasks.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | str | yes | `create`, `list`, `remove` |
| `schedule` | str | cond. | Cron expression (required for create) |
| `task` | str | cond. | Task description (required for create) |
| `job_id` | str | cond. | Job ID (required for remove) |

```yaml
tool: cronjob
args:
  action: create
  schedule: "0 9 * * 1-5"
  task: "Check email and summarize unread messages"
```

### delegate

Spawn a sub-agent to handle a task independently.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task` | str | yes | Task description for the sub-agent |
| `tools` | list | no | Tools available to the sub-agent |
| `timeout` | int | no | Max time in seconds |
| `context` | str | no | Additional context to pass |

### document

Generate structured documents in various formats.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | str | yes | Document content (markdown) |
| `format` | str | no | Output format: `md`, `pdf`, `html`, `docx` |
| `filename` | str | no | Output filename |

### filesystem

Perform file system operations: read, write, list, delete, move.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | str | yes | `read`, `write`, `list`, `delete`, `move`, `copy`, `mkdir` |
| `path` | str | yes | Target file or directory path |
| `content` | str | cond. | File content (required for write) |
| `dest` | str | cond. | Destination path (required for move/copy) |

### image_gen

Generate images using the configured default provider.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | str | yes | Image description |
| `size` | str | no | Dimensions: `256x256`, `512x512`, `1024x1024` |
| `style` | str | no | Style hint: `natural`, `artistic`, `diagram` |

### image_gen_fal

Generate images specifically via Fal.ai backend.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | str | yes | Image description |
| `model` | str | no | Fal model identifier |
| `size` | str | no | Output dimensions |
| `num_images` | int | no | Number of images (default: 1) |

### knowledge

Query the knowledge base for relevant documents.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | str | yes | Search query |
| `top_k` | int | no | Number of results (default: 5) |
| `filter` | dict | no | Metadata filters |
| `scope` | str | no | `global`, `workspace`, `all` |

### memory

Read, write, or search persistent agent memory.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | str | yes | `read`, `write`, `search`, `delete` |
| `key` | str | cond. | Memory key (for read/write/delete) |
| `value` | str | cond. | Value to store (for write) |
| `query` | str | cond. | Search query (for search) |

### message

Send a message to the user via the active channel.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | str | yes | Message text |
| `format` | str | no | Format: `text`, `markdown`, `html` |
| `reply_to` | str | no | Message ID to reply to |

### notify

Send a notification/alert to the user.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `title` | str | yes | Notification title |
| `body` | str | no | Notification body |
| `priority` | str | no | `low`, `normal`, `high` |
| `channel` | str | no | Target channel for notification |

### patch

Apply a unified diff patch to one or more files.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `diff` | str | yes | Unified diff content |
| `base_dir` | str | no | Base directory for relative paths |
| `dry_run` | bool | no | Validate without applying |

### process

Start, stop, or signal system processes.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | str | yes | `start`, `stop`, `signal`, `list`, `info` |
| `command` | str | cond. | Command to start |
| `pid` | int | cond. | Process ID (for stop/signal) |
| `signal` | str | no | Signal name (default: SIGTERM) |

### read_spill

Read content that was spilled to disk due to size constraints.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `spill_id` | str | yes | Spill file identifier |
| `offset` | int | no | Start byte offset |
| `length` | int | no | Bytes to read |

### search

Perform a web search and return results.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | str | yes | Search query |
| `num_results` | int | no | Number of results (default: 10) |
| `provider` | str | no | Search provider override |

### send_file

Send a file to the user via the active channel.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | str | yes | File path to send |
| `caption` | str | no | File caption/description |
| `filename` | str | no | Override display filename |

### session_search

Search across past conversation sessions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | str | yes | Search query |
| `limit` | int | no | Max results (default: 10) |
| `date_from` | str | no | Start date filter (ISO 8601) |
| `date_to` | str | no | End date filter (ISO 8601) |

### shell

Execute shell commands with full system access.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | str | yes | Shell command to execute |
| `cwd` | str | no | Working directory |
| `timeout` | int | no | Timeout in seconds (default: 60) |
| `env` | dict | no | Additional environment variables |

!!! warning "Shell vs code_exec"
    `shell` has full system access (network, filesystem, processes). Use `code_exec` for isolated computation. `shell` defaults to `ask` approval in `coding` profile.

### skill_install

Install a skill from an external source.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `source` | str | yes | Skill source (URL, path, or registry name) |
| `version` | str | no | Specific version to install |
| `force` | bool | no | Overwrite existing skill |

### skill_run

Execute an installed skill.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `skill` | str | yes | Skill name |
| `args` | dict | no | Arguments to pass to the skill |
| `timeout` | int | no | Execution timeout |

### skills

List available skills and their status.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `filter` | str | no | Filter by name pattern |
| `status` | str | no | Filter by status: `active`, `staged`, `disabled` |

### task

Add a task to the agent's task queue for deferred execution.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | str | yes | Task description |
| `priority` | str | no | `low`, `normal`, `high`, `critical` |
| `due` | str | no | Due date/time (ISO 8601) |
| `depends_on` | list | no | Task IDs this depends on |

### todo

Manage simple todo/checklist items.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | str | yes | `add`, `list`, `complete`, `remove` |
| `text` | str | cond. | Todo text (for add) |
| `id` | str | cond. | Todo ID (for complete/remove) |

### tts

Convert text to speech audio.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `text` | str | yes | Text to synthesize |
| `voice` | str | no | Voice identifier |
| `format` | str | no | Audio format: `mp3`, `ogg`, `wav` |
| `speed` | float | no | Speech rate multiplier |

### vision

Analyze an image and describe or extract information.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `image` | str | yes | Image path, URL, or base64 data |
| `prompt` | str | no | Specific question about the image |
| `detail` | str | no | Detail level: `low`, `high` |

### web

Fetch and process web URL content.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | str | yes | URL to fetch |
| `method` | str | no | HTTP method (default: GET) |
| `headers` | dict | no | Request headers |
| `body` | str | no | Request body |
| `extract` | str | no | Content extraction mode: `text`, `html`, `json` |

### workflow

Execute a multi-step workflow definition.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `steps` | list | yes | List of workflow step definitions |
| `parallel` | bool | no | Run independent steps in parallel |
| `on_error` | str | no | Error handling: `stop`, `skip`, `retry` |

---

## Profile Matrix

Complete mapping of tools to profiles and default approval modes:

| Tool | minimal | messaging | coding | full |
|------|---------|-----------|--------|------|
| browser | auto | auto | auto | auto |
| clarify | auto | auto | auto | auto |
| code_exec | deny | deny | ask | auto |
| cronjob | deny | deny | deny | ask |
| delegate | deny | deny | auto | auto |
| document | deny | deny | auto | auto |
| filesystem | deny | deny | ask | auto |
| image_gen | deny | auto | auto | auto |
| image_gen_fal | deny | auto | auto | auto |
| knowledge | auto | auto | auto | auto |
| memory | auto | auto | auto | auto |
| message | deny | auto | auto | auto |
| notify | deny | auto | auto | auto |
| patch | deny | deny | ask | auto |
| process | deny | deny | deny | ask |
| read_spill | auto | auto | auto | auto |
| search | auto | auto | auto | auto |
| send_file | deny | auto | auto | auto |
| session_search | auto | auto | auto | auto |
| shell | deny | deny | ask | ask |
| skill_install | deny | deny | deny | ask |
| skill_run | deny | deny | auto | auto |
| skills | auto | auto | auto | auto |
| task | deny | deny | auto | auto |
| todo | auto | auto | auto | auto |
| tts | deny | auto | auto | auto |
| vision | auto | auto | auto | auto |
| web | auto | auto | auto | auto |
| workflow | deny | deny | ask | auto |

!!! question "Maintainer confirmation needed"
    Are the default approval modes in the profile matrix above accurate? In particular, should `shell` remain `ask` even in the `full` profile?
