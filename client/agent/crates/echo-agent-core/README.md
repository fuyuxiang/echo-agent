# `echo-agent-core`

Agent builder, definition parsing, and system prompt assembly.

This crate extracts a first-class `Agent` type from `echo-agent-runtime`.
An `Agent` bundles tools, system prompt, system-reminder policy,
compaction policy, and model configuration into a single, portable
object that any host can consume — whether that host is
`echo-agent-runtime`, another in-process host, or a headless batch runner.

## Quick Start

### From a definition file

Agent definitions are **Markdown files with YAML frontmatter**, stored
in `.echo-agent/agents/` (project-level) or `~/.echo-agent/agents/` (user-level).

```rust
use echo_agent_core::{AgentDefinition, AgentBuilder};
use echo_agent_tools::notification::ToolNotificationHandle;

// 1. Parse the definition file
let def = AgentDefinition::from_file(".echo-agent/agents/code-reviewer.md")?;

// 2. Build the agent
let agent = AgentBuilder::new(cwd, None, ToolNotificationHandle::noop())
    .from_definition(def)
    .build()
    .await?;

// 3. Use it
println!("Agent: {}", agent.name());
println!("Prompt: {}", agent.system_prompt());
let tool_defs = agent.tool_definitions().await;
```

### Programmatic (no file)

```rust
let agent = AgentBuilder::new(cwd, None, ToolNotificationHandle::noop())
    .with_name("my-agent")
    .with_description("A custom agent")
    .with_tools(vec!["read_file".into(), "grep".into()])
    .build()
    .await?;
```

### Discover all definitions

```rust
use echo_agent_core::discovery;

// Find all .md files in .echo-agent/agents/ directories
let definitions = discovery::discover(&cwd);

// Find a specific agent by name (checks built-ins, then user dirs)
let reviewer = discovery::by_name("code-reviewer");

// Find with project-level priority
let agent = discovery::by_name_in_cwd("my-agent", &cwd);
```

## Agent Definition File Format

Agent definitions are Markdown files with YAML frontmatter:

```markdown
---
name: my-agent
description: What this agent does
# ... additional config fields
---

System prompt body goes here...
```

The **frontmatter** (between `---` delimiters) is YAML configuration.
The **body** (after the closing `---`) is the system prompt content.

### Minimal example (extends base template)

```markdown
---
name: code-reviewer
description: Reviews code for quality and security
tools:
  - read_file
  - grep
  - list_dir
permissionMode: plan
---

You are a senior code reviewer. Analyze code and provide
actionable feedback organized by severity.
```

With `promptMode: extend` (the default), the body is appended to the
base template which includes tool calling conventions, formatting
rules, and user info. The author only writes persona-specific content.

### Full prompt override

```markdown
---
name: custom-agent
description: Agent with full control over the system prompt
promptMode: full
tools:
  - read_file
  - search_replace
  - run_terminal_cmd
---

You are a custom agent.

Use ${{ tools.read_file }} to read files.
Use ${{ tools.search_replace }} to edit files.

${%- if tools.run_terminal_cmd %}
Use ${{ tools.run_terminal_cmd }} for shell commands.
${%- endif %}

<user_info>
OS: ${{ os_name }}
Shell: ${{ shell_path }}
Working Directory: ${{ working_directory }}
Date: ${{ current_date }}
</user_info>
```

With `promptMode: full`, the body IS the complete system prompt,
rendered through MiniJinja with custom `${{ }}`/`${% %}` delimiters
(to avoid collisions with literal `{{ }}` in prose).

### With completion requirement (orchestrated mode)

```markdown
---
name: orchestrator-worker
description: Worker agent that must signal completion before ending a turn
completionRequirement:
  tool: complete_task
  reminder: >
    You stopped without calling `complete_task`.
    Please continue and call it when done.
  recovery:
    maxRetries: 5
    baseDelayMs: 5000
    maxDelayMs: 60000
toolConfig:
  wait_for_instruction:
    retry:
      maxRetries: 1440
      baseDelayMs: 5000
      maxDelayMs: 30000
---

You are a worker agent in an orchestrated multi-agent workflow.
You MUST call `complete_task` before ending your response.
```

## Frontmatter Schema Reference

All frontmatter keys use **camelCase**.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | `string` | **Yes** | — | Unique agent ID (lowercase, hyphens) |
| `description` | `string` | **Yes** | — | When/why to use this agent |
| `promptMode` | `string` | No | `"extend"` | `"extend"` or `"full"` |
| `tools` | `string[]` | No | inherit all | Tool allowlist. Omit = all tools. `[]` = none |
| `disallowedTools` | `string[]` | No | `[]` | Denylist (takes priority over `tools`) |
| `permissionMode` | `string` | No | `"default"` | `"default"`, `"acceptEdits"`, `"dontAsk"`, `"plan"` |
| `skills` | `string[]` | No | `[]` | Skill names to pre-load |
| `agentsMd` | `bool` | No | `true` | Discover and inject AGENTS.md files |
| `outputFormat` | `string` | No | `"default"` | `"default"` or `"concise"` |
| `bash` | `object` | No | defaults | Bash tool config overrides |
| `bash.timeoutSecs` | `float` | No | `120.0` | Bash command timeout |
| `bash.outputByteLimit` | `int` | No | `200000` | Max output bytes |
| `bash.cmdPrefix` | `string` | No | `null` | Command prefix |
| `toolNameOverrides` | `map<string,string>` | No | `{}` | Canonical → model-facing name map |
| `paramNameOverrides` | `map<string,map>` | No | `{}` | Per-tool param name map |
| `completionRequirement` | `object` | No | `null` | Tool that must be called before turn ends |
| `completionRequirement.tool` | `string` | Yes* | — | Canonical tool name |
| `completionRequirement.reminder` | `string` | Yes* | — | Reminder text when not called |
| `completionRequirement.recovery` | `object` | No | `null` | Recovery policy for the harness |
| `toolConfig` | `map<string,object>` | No | `{}` | Per-tool execution config |
| `toolConfig.*.retry` | `object` | No | `null` | Retry config for a tool |

*Required only when `completionRequirement` is set.

## Prompt Assembly

```
promptMode: extend                     promptMode: full
──────────────────                     ─────────────────
1. Base template (MiniJinja)           1. Markdown body (MiniJinja, ${{ }}/${% %})
   (tool conventions, formatting,      2. AGENTS.md section (if agentsMd: true)
    user_info, background tasks)       3. Skills section
2. Markdown body (appended raw)
3. AGENTS.md section (if agentsMd: true)
4. Skills section
```

### Template Variables (full mode)

| Variable | Description |
|---|---|
| `${{ tools.read_file }}` | Resolved name for `read_file` (or empty if disabled) |
| `${{ tools.search_replace }}` | Resolved name for `search_replace` |
| `${{ tools.run_terminal_cmd }}` | Resolved name for `run_terminal_cmd` |
| `${{ tools.grep }}` | Resolved name for `grep` |
| `${{ tools.list_dir }}` | Resolved name for `list_dir` |
| `${{ tools.todo_write }}` | Resolved name for `todo_write` |
| `${{ tools.skill }}` | Resolved name for `skill` |
| `${{ tools.get_task_output }}` | Resolved name for `get_task_output` |
| `${{ tools.kill_task }}` | Resolved name for `kill_task` |
| `${{ tools.web_search }}` | Resolved name for `web_search` |
| `${{ os_name }}` | Operating system (e.g. `"macos"`, `"linux"`) |
| `${{ shell_path }}` | Shell path (e.g. `"/bin/zsh"`) |
| `${{ working_directory }}` | Workspace path |
| `${{ current_date }}` | Current date in the user's local timezone (`YYYY-MM-DD`) |

Conditionals: `${%- if tools.todo_write %}...${%- endif %}` — block
is omitted when the tool is disabled.

## Discovery Rules

Agent definitions are discovered from multiple locations with priority:

1. **Project-level** (highest priority): `.echo-agent/agents/*.md` — walk
   from `cwd` up to the git repository root. Files found closer to
   `cwd` take priority.
2. **User-level**: `~/.echo-agent/agents/*.md`
3. **Compat paths** (lowest priority): additional vendor agent
   directories under the user home (when enabled)
4. **Built-in**: `default_echo_agent_build()`, `browser_use()`

Name-based dedup ensures the highest-priority definition wins. For
example, a project `.echo-agent/agents/code-reviewer.md` shadows a
user-level definition with the same name.

## Crate Relationships

```
┌──────────────────┐
│  echo-agent-core  │  ← This crate
│  (Agent, Builder, │
│   Definition)     │
└────────┬─────────┘
         │ depends on
         ▼
┌──────────────────┐
│  echo-agent-tools  │
│  (ToolBridge,    │
│   ToolRegistry,  │
│   ToolState)     │
└────────▲─────────┘
         │ depends on
┌────────┴─────────┐
│  echo-agent-runtime  │  uses AgentBuilder to create
│  (session host)  │  Agent during session setup
└──────────────────┘
```

- **`echo-agent-tools`**: Provides `ToolBridge`, `ToolRegistry`,
  `ToolState`, `SystemReminderLayer`, and tool implementations.
  `echo-agent-core` depends on it for tool setup.
- **`echo-agent-runtime`**: The application shell. Uses `AgentBuilder`
  to construct an `Agent` during session creation. The shell
  re-exports some modules from `echo-agent-core` (AGENTS.md
  discovery, skills discovery, base prompt rendering).

## Built-in Agents

| Name | Prompt Mode | Description |
|---|---|---|
| `echo-agent-build` | extend | Default agent for software engineering tasks |
| `browser-use` | full | Web browsing and interaction agent |

## Error Handling

`AgentBuilder::build()` returns `Result<Agent, AgentBuildError>`:

| Error | When |
|---|---|
| `ParseError` | Bad YAML, missing `---`, wrong types |
| `MissingField` | Required field (`name`/`description`) absent |
| `UnknownToolOverride` | `toolNameOverrides` references nonexistent tool |
| `IoError` | File read error during AGENTS.md/skills discovery |
| `MiniJinjaError` | Template rendering failure |

Unknown frontmatter fields are **silently ignored** for forward
compatibility — definitions written for newer versions work on older
ones.

## Development

```bash
# Check
cargo check -p echo-agent-core

# Test
cargo test -p echo-agent-core

# Clippy
cargo clippy -p echo-agent-core --fix --allow-dirty

# Format
cargo fmt --all
```


