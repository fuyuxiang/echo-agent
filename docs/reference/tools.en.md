# Built-in Tool Reference

Echo Agent ships 36 built-in tools. The tool names, parameters and capability tags on this page are taken from the registrations in code: a tool's name is the `name` attribute of its class (under `echo_agent/agent/tools/`), capability tags come from `TOOL_CAPABILITIES` in `echo_agent/security/capabilities.py`, and profile membership comes from `echo_agent/security/tool_policy.py`.

!!! note "A tool's name is not its module name"
    Tool names frequently differ from the file that implements them, and calls must use the tool name. For example `shell.py` registers `exec`, `code_exec.py` registers `execute_code`, `tts.py` registers `text_to_speech`, and `web.py` registers both `web_fetch` and `web_search`.

## Two different `profile` fields

Two configuration fields are named `profile`. They do different things and must not be conflated:

| Option | Values | Default | Purpose |
|--------|--------|---------|---------|
| `tools.profile` | `minimal` / `messaging` / `coding` / `full` | `full` | Which tools are exposed to the model |
| `security.profile` | `personal_cli` / `daemon` / `public_gateway` | `personal_cli` | The overall security baseline for the deployment shape |

The four `tools.profile` tiers are cumulative allowlists; `full` has the allowlist `*`, meaning every tool is permitted. See the [security profile matrix](security-profile-matrix.md).

## Tool availability by profile

The table below shows which `tools.profile` tiers expose each tool. `full` permits all of them and is therefore not listed separately.

| Tool | Module | minimal | messaging | coding | Capability tags |
|------|--------|:-------:|:---------:|:------:|-----------------|
| `agents_list` | none (see below) | ✅ | ✅ | ✅ | `agent.read` |
| `agents_route` | none (see below) | ✅ | ✅ | ✅ | `agent.dispatch` |
| `clarify` | `clarify.py` | ✅ | ✅ | ✅ | `message.ask` |
| `knowledge_search` | `knowledge.py` | ✅ | ✅ | ✅ | `knowledge.read` |
| `list_dir` | `filesystem.py` | ✅ | ✅ | ✅ | `fs.read` |
| `message` | `message.py` | ✅ | ✅ | ✅ | `message.send` |
| `notify` | `notify.py` | ✅ | ✅ | ✅ | `message.send` |
| `read_file` | `filesystem.py` | ✅ | ✅ | ✅ | `fs.read` |
| `read_spill` | `read_spill.py` | ✅ | ✅ | ✅ | `fs.read` |
| `search_files` | `search.py` | ✅ | ✅ | ✅ | `fs.read` |
| `session_search` | `session_search.py` | ✅ | ✅ | ✅ | `session.read` |
| `skill_view` | `skills.py` | ✅ | ✅ | ✅ | `skill.read` |
| `skills_list` | `skills.py` | ✅ | ✅ | ✅ | `skill.read` |
| `todo` | `todo.py` | ✅ | ✅ | ✅ | `task.write` |
| `image_generate` | `image_gen_fal.py` | ❌ | ✅ | ✅ | `media.generate` `network.outbound` |
| `memory` | `memory.py` | ❌ | ✅ | ✅ | `memory.read` `memory.write` |
| `text_to_speech` | `tts.py` | ❌ | ✅ | ✅ | `media.generate` `network.outbound` |
| `vision_analyze` | `vision.py` | ❌ | ✅ | ✅ | `media.read` |
| `edit_file` | `filesystem.py` | ❌ | ❌ | ✅ | `fs.read` `fs.write` |
| `knowledge_index` | `knowledge.py` | ❌ | ❌ | ✅ | `knowledge.write` `fs.read` |
| `patch` | `patch.py` | ❌ | ❌ | ✅ | `fs.read` `fs.write` |
| `task` | `task.py` | ❌ | ❌ | ✅ | `task.write` |
| `workflow` | `workflow.py` | ❌ | ❌ | ✅ | `workflow.write` |
| `write_file` | `filesystem.py` | ❌ | ❌ | ✅ | `fs.write` |
| `browser` | `browser.py` | ❌ | ❌ | ❌ | unclassified |
| `cronjob` | `cronjob.py` | ❌ | ❌ | ❌ | `scheduler.write` |
| `delegate_task` | `delegate.py` | ❌ | ❌ | ❌ | unclassified |
| `exec` | `shell.py` | ❌ | ❌ | ❌ | `process.exec` |
| `execute_code` | `code_exec.py` | ❌ | ❌ | ❌ | `code.exec` `process.exec` |
| `process` | `process.py` | ❌ | ❌ | ❌ | `process.exec` `process.manage` |
| `read_document` | `document.py` | ❌ | ❌ | ❌ | unclassified |
| `send_file` | `send_file.py` | ❌ | ❌ | ❌ | unclassified |
| `skill_install` | `skill_install.py` | ❌ | ❌ | ❌ | `skill.install` `network.outbound` `fs.write` |
| `skill_manage` | `skills.py` | ❌ | ❌ | ❌ | `skill.write` `fs.write` |
| `skill_run` | `skill_run.py` | ❌ | ❌ | ❌ | unclassified |
| `spawn_task` | `delegate.py` | ❌ | ❌ | ❌ | unclassified |
| `web_fetch` | `web.py` | ❌ | ❌ | ❌ | `network.outbound` |
| `web_search` | `web.py` | ❌ | ❌ | ❌ | `network.outbound` |

Tools marked "unclassified" have no entry in `TOOL_CAPABILITIES`, so `tool_capabilities()` returns an empty set for them and capability-based deny rules never match. Such tools are constrained only by rules that name them directly.

`agents_list` and `agents_route` appear only in the policy tables (`capabilities.py`, `tool_policy.py`, `risk_classifier.py`); there is no implementation under `echo_agent/agent/tools/`, so they cannot currently be called. They are reserved names for multi-agent collaboration, and this page does not document parameters for them. Every one of the 36 implemented tools is documented below.

## High-risk tools

`HIGH_RISK_TOOLS` is a set that exists independently of the tier system, with 6 members: `cronjob`, `exec`, `execute_code`, `process`, `skill_install`, `skill_manage`.

Two deployment shapes add further restrictions on top:

- **`public_gateway`** — beyond the 6 high-risk tools, also denies `edit_file`, `knowledge_index`, `patch`, `workflow` and `write_file` (11 in total), plus the capabilities `code.exec`, `fs.write`, `process.exec`, `process.manage`, `scheduler.write`, `skill.install`, `skill.write` and `workflow.write`.
- **`daemon`** — denies `exec`, `execute_code`, `process` and `skill_install` by default, plus the capabilities `code.exec`, `process.exec`, `process.manage` and `skill.install`.

## Tool details

Parameters marked `*` are required. Types and defaults come from each tool's `parameters` JSON Schema.

### Files and search

#### read_file

Read the contents of a file.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `path` | string | ✅ | — | File path |
| `offset` | integer | | — | Starting line |
| `limit` | integer | | — | Number of lines to read |

#### write_file

Write content to a file, creating it if needed.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `path` | string | ✅ | File path |
| `content` | string | ✅ | Content to write |

#### edit_file

Replace a string in a file.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `path` | string | ✅ | — | File path |
| `old_string` | string | ✅ | — | String to replace |
| `new_string` | string | ✅ | — | Replacement string |
| `replace_all` | boolean | | `false` | Replace every occurrence |

#### patch

Apply a patch to a file using unified diff format or search-and-replace blocks.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `file_path` | string | ✅ | — | Target file path |
| `patch` | string | ✅ | — | Patch content |
| `fuzzy_threshold` | number | | `0.6` | Fuzzy match threshold |

#### list_dir

List files and directories at a path.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `path` | string | ✅ | Directory path |

#### search_files

Search file contents by regex, or find files by glob pattern.

| Parameter | Type | Required | Default | Values | Description |
|-----------|------|:--------:|---------|--------|-------------|
| `pattern` | string | ✅ | — | — | Search pattern |
| `mode` | string | | — | `content` \| `glob` | Search contents or match filenames |
| `path` | string | | — | — | Search scope |
| `max_results` | integer | | `50` | — | Maximum results |

#### read_document

Read text from a document file: pdf, docx, xlsx, pptx, txt, csv or md.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `path` | string | ✅ | Document path |
| `max_chars` | integer | | Maximum characters to read |
| `unit` | integer \| string | | Read unit |

#### read_spill

Retrieve a tool output artifact that was spilled to disk, using the path given in the tool result.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `path` | string | ✅ | Spill artifact path |
| `offset` | integer | | Starting position |
| `limit` | integer | | Length to read |
| `pattern` | string | | Filter pattern |

### Knowledge and memory

#### knowledge_search

Search the local knowledge base and return cited snippets.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `query` | string | ✅ | — | Query text |
| `max_results` | integer | | `5` | Number of results |

#### knowledge_index

Inspect or rebuild the local knowledge index.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `status` \| `rebuild` | Inspect status or rebuild |

#### memory

Manage persistent memory across sessions.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `add` \| `replace` \| `remove` \| `search` \| `list` \| `list_contradictions` \| `resolve_contradiction` | Operation |
| `content` | string | | — | Memory content |
| `key` | string | | — | Memory key |
| `old_text` | string | | — | Text being replaced |
| `query` | string | | — | Search keywords |
| `tags` | string | | — | Tags |
| `importance` | number | | — | Importance score |
| `pinned` | boolean | | — | Whether pinned |
| `source` | string | | `user_stated` \| `model_inferred` | Where the memory came from |
| `target` | string | | `user` \| `environment` | What the memory is about |
| `contradiction_id` | string | | — | Contradiction record ID |
| `winner_id` | string | | — | Which side to keep when resolving |

#### session_search

Search past conversation messages across sessions by keyword or regex.

| Parameter | Type | Required | Default | Values | Description |
|-----------|------|:--------:|---------|--------|-------------|
| `query` | string | ✅ | — | — | Query text |
| `max_results` | integer | | `20` | — | Maximum results |
| `role_filter` | string | | — | `user` \| `assistant` \| `all` | Filter by role |
| `session_key` | string | | — | — | Restrict to one session |

### Messaging and media

#### message

Send a message to a specific channel and chat.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `channel` | string | ✅ | Target channel registry name |
| `chat_id` | string | ✅ | Target chat ID |
| `text` | string | ✅ | Message body |

#### notify

Send a notification to a specific channel or the current chat. Unlike `message`, both channel and chat may be omitted, in which case the message goes to the current chat.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `message` | string | ✅ | Notification body |
| `channel` | string | | Target channel |
| `chat_id` | string | | Target chat ID |

#### send_file

Send a local file or image to a specific channel and chat.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `channel` | string | ✅ | Target channel registry name |
| `chat_id` | string | ✅ | Target chat ID |
| `file_path` | string | ✅ | Local file path |
| `caption` | string | | Accompanying caption |
| `as_image` | boolean | | Send as an image |

Check the target channel's `supports_files` before calling, otherwise the file cannot be delivered. Today only `weixin` supports it unconditionally; `qqbot` depends on its `media_enabled` setting. See [message channels](../integrations/channels/index.md).

#### clarify

Ask the user a clarifying question, optionally with multiple-choice options.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `question` | string | ✅ | The question to ask |
| `options` | array | | Options for the user to choose from |

#### image_generate

Generate an image from a text prompt.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `prompt` | string | ✅ | — | Image description |
| `aspect_ratio` | string | | `landscape` \| `square` \| `portrait` | Aspect ratio |

`image_gen.py` and `image_gen_fal.py` register the same tool name; which one is active depends on configuration.

#### text_to_speech

Convert text to speech audio. Uses edge-tts by default, or OpenAI TTS when configured.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `text` | string | ✅ | — | Text to synthesise |
| `backend` | string | | `edge` \| `openai` | Synthesis backend |
| `voice` | string | | — | Voice |
| `output_path` | string | | — | Output file path |
| `deliver` | boolean | | — | Deliver directly |
| `deliver_channel` | string | | — | Delivery channel |
| `deliver_chat_id` | string | | — | Delivery chat ID |
| `caption` | string | | — | Accompanying caption |

#### vision_analyze

Analyse an image with a vision-capable model. Accepts a local path or a URL.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `image` | string | ✅ | Image path or URL |
| `prompt` | string | ✅ | Question about the image |
| `model` | string | | Model to use |

### Network access

#### web_search

Search the web for information.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `query` | string | ✅ | — | Query text |
| `max_results` | integer | | `5` | Maximum results |

#### web_fetch

Fetch content from a URL.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `url` | string | ✅ | Target URL |
| `max_chars` | integer | | Maximum characters to read |

#### browser

Drive a real browser for multi-step web interaction. Sessions are tracked by `session_id`, and elements are addressed by the `ref` values from a snapshot.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `open` `navigate` `snapshot` `click` `type` `press` `scroll` `back` `forward` `reload` `hover` `select` `upload` `wait` `evaluate` `console` `screenshot` `get_images` `close` | Operation |
| `session_id` | string | | — | Browser session ID |
| `url` | string | | — | Target URL |
| `ref` | string | | — | Element reference from a snapshot |
| `text` | string | | — | Text to type |
| `key` | string | | — | Key name |
| `press_enter` | boolean | | — | Press Enter after typing |
| `direction` | string | | `up` \| `down` \| `left` \| `right` \| `top` \| `bottom` | Scroll direction |
| `amount` | integer | | — | Scroll distance |
| `values` | array | | — | Values for a select control |
| `paths` | array | | — | File paths to upload |
| `expression` | string | | — | Expression to evaluate |
| `state` | string | | `load` \| `domcontentloaded` \| `networkidle` | Page state to wait for |
| `full_page` | boolean | | — | Capture the full page |
| `timeout_sec` | integer | | — | Timeout in seconds |

All outbound requests pass the SSRF policy in `echo_agent/security/net_guard.py`; hosts that cannot be resolved are rejected.

### Execution and processes

Every tool in this section belongs to `HIGH_RISK_TOOLS`.

#### exec

Execute a shell command in the workspace.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `command` | string | ✅ | — | Command to run |
| `cwd` | string | | — | Working directory |
| `timeout` | integer | | `30` | Timeout in seconds |

#### execute_code

Execute a code snippet in a sandboxed subprocess.

| Parameter | Type | Required | Default | Values | Description |
|-----------|------|:--------:|---------|--------|-------------|
| `code` | string | ✅ | — | — | Code to run |
| `language` | string | ✅ | — | `python` \| `javascript` \| `bash` | Language |
| `timeout` | integer | | `30` | — | Timeout in seconds |

#### process

Manage background processes.

| Parameter | Type | Required | Default | Values | Description |
|-----------|------|:--------:|---------|--------|-------------|
| `action` | string | ✅ | — | `start` \| `list` \| `poll` \| `stop` | Operation |
| `command` | string | | — | — | Command to start |
| `process_id` | string | | — | — | Process ID |
| `timeout` | integer | | `300` | — | Timeout in seconds |

### Tasks and orchestration

#### todo

Manage a task list for planning multi-step work.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `create` \| `update` \| `list` \| `complete` \| `delete` | Operation |
| `title` | string | | — | Title |
| `items` | array | | — | List items |
| `task_id` | string | | — | Item ID |
| `status` | string | | `pending` \| `in_progress` \| `done` | Status |
| `notes` | string | | — | Notes |

#### task

Manage tasks with full lifecycle tracking.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `create` \| `list` \| `get` \| `start` \| `complete` \| `fail` \| `cancel` \| `retry` \| `update` | Operation |
| `title` | string | | — | Title |
| `description` | string | | — | Description |
| `task_id` | string | | — | Task ID |
| `priority` | integer | | — | Priority |
| `status_filter` | string | | — | Filter for listing |
| `result` | string | | — | Completion result |
| `error` | string | | — | Failure reason |
| `workflow_id` | string | | — | Associated workflow ID |

#### workflow

Orchestrate multi-step workflows with DAG-based step dependencies. This engine only orchestrates; it does not execute business logic.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `create` \| `start` \| `status` \| `advance` \| `pause` \| `resume` \| `cancel` \| `list` | Operation |
| `name` | string | | — | Workflow name |
| `description` | string | | — | Description |
| `steps` | array | | — | Step definitions |
| `workflow_id` | string | | — | Workflow ID |
| `status_filter` | string | | — | Filter for listing |

#### cronjob

Manage scheduled jobs. Belongs to `HIGH_RISK_TOOLS`.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `create` \| `list` \| `delete` \| `trigger` \| `authorize` \| `revoke` | Operation |
| `name` | string | | — | Job name |
| `schedule` | string | | — | cron expression |
| `command` | string | | — | What to run |
| `job_id` | string | | — | Job ID (required for `delete`/`trigger`/`authorize`/`revoke`) |
| `target_channel` | string | | — | Channel for results |
| `target_chat_id` | string | | — | Chat for results |

`authorize` grants unattended authorization to an **existing** job. Editing a
job's content invalidates its authorization automatically (see
[Scheduled jobs](../guides/scheduled-jobs.en.md)), so re-authorizing is a routine
operation. Because `cronjob` belongs to `HIGH_RISK_TOOLS`, `authorize` always
raises a human confirmation prompt listing the job's instruction, schedule and
delivery target before the grant is issued. `revoke` withdraws it and needs no
extra confirmation.

#### delegate_task

Delegate subtasks to worker agents for parallel or isolated execution.

| Parameter | Type | Required | Default | Description |
|-----------|------|:--------:|---------|-------------|
| `goal` | string | | — | Overall goal |
| `tasks` | array | | — | Subtask list |
| `tools` | array | | — | Tools the worker may use |
| `worker_profile` | string | | — | Worker profile |
| `max_iterations` | integer | | `12` | Maximum iterations |

#### spawn_task

Spawn a background worker that runs asynchronously and can use tools.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `task` | string | ✅ | Task description |
| `context` | string | | Additional context |

### Skill management

#### skills_list

List all available skills with compact metadata. This tool takes no parameters.

#### skill_view

View a skill's full content (`SKILL.md`) or one of its supporting files. Without `file_path`, returns `SKILL.md`.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `name` | string | ✅ | Skill name |
| `file_path` | string | | Path within the skill |

#### skill_run

Run a skill's script with the agent's own Python interpreter and the working directory locked to the skill's directory.

| Parameter | Type | Required | Description |
|-----------|------|:--------:|-------------|
| `name` | string | ✅ | Skill name |
| `script` | string | ✅ | Script path |
| `args` | array | | Command-line arguments |
| `timeout` | integer | | Timeout in seconds |

#### skill_manage

Create, edit, patch or delete skills. Belongs to `HIGH_RISK_TOOLS`.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `action` | string | ✅ | `create` \| `edit` \| `patch` \| `delete` \| `write_file` \| `remove_file` | Operation |
| `name` | string | ✅ | — | Skill name |
| `category` | string | | — | Category |
| `content` | string | | — | Content |
| `file_path` | string | | — | Path within the skill |
| `old_text` | string | | — | Text being replaced |
| `new_text` | string | | — | Replacement text |

#### skill_install

Install a skill from an external source into the local skill store. Belongs to `HIGH_RISK_TOOLS`.

| Parameter | Type | Required | Values | Description |
|-----------|------|:--------:|--------|-------------|
| `source` | string | ✅ | `git` \| `local` \| `url` | Source type |
| `location` | string | ✅ | — | Source address |
| `name` | string | | — | Name to install as |
| `subdirectory` | string | | — | Subdirectory within the source |
| `run_install` | boolean | | — | Run the install script |

## Related pages

- [Security profile matrix](security-profile-matrix.md) — full mapping of profiles and deployment shapes
- [Configuration reference](configuration.md) — per-option reference generated from the schema
- [Message channels](../integrations/channels/index.md) — channel capabilities and delivery limits
