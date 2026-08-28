# Tools & Permissions

Echo Agent extends language model capabilities through an extensible tool system. Each tool declares a risk level and capabilities; the Approval Gate decides at runtime whether a call may proceed.

---

## Tool Base Class

All tools inherit from the public `echo_agent.tools.Tool` contract with these core attributes:

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `name` | `str` | `""` | Unique tool identifier |
| `description` | `str` | `""` | Function description (injected into model context) |
| `parameters` | `dict` | `{}` | JSON Schema parameter definition |
| `timeout_seconds` | `int` | `30` | Execution timeout per call |
| `max_retries` | `int` | `0` | Retry count on failure |
| `stream_capable` | `bool` | `False` | Whether tool supports streaming output |
| `capabilities` | `tuple[str, ...]` | `()` | Capability declarations (override static map) |
| `risk_level` | `str` | `"write"` | Risk level |

Runtime readiness checks:

- `is_ready() -> bool`: Whether external dependencies are available (e.g. API key configured)
- `readiness_detail() -> tuple[bool, str]`: Detailed readiness status and reason

---

## Complete Tool Catalog

### Read-Only Tools (read_only)

| Tool | Description | Capabilities |
|------|-------------|--------------|
| `read_file` | Read file contents | `fs.read` |
| `list_dir` | List directory contents | `fs.read` |
| `search_files` | Search file contents | `fs.read` |
| `knowledge_search` | Knowledge base retrieval | `knowledge.read` |
| `session_search` | Search conversation history | `session.read` |
| `skills_list` | List installed skills | `skill.read` |
| `skill_view` | View skill details | `skill.read` |
| `agents_list` | List sub-agents | `agent.read` |
| `agents_route` | Route to sub-agent | `agent.dispatch` |
| `web_fetch` | Fetch web page content | `network.outbound` |
| `web_search` | Web search | `network.outbound` |
| `vision_analyze` | Image analysis | `media.read` |
| `read_spill` | Read spilled content | `fs.read` |

### Write Tools (write)

| Tool | Description | Capabilities |
|------|-------------|--------------|
| `write_file` | Write file | `fs.write` |
| `edit_file` | Edit file | `fs.read`, `fs.write` |
| `patch` | Patch-style file modification | `fs.read`, `fs.write` |
| `knowledge_index` | Build knowledge base index | `knowledge.write`, `fs.read` |
| `todo` | Task list management | `task.write` |
| `task` | Task creation and scheduling | `task.write` |
| `workflow` | Workflow orchestration | `workflow.write` |
| `notify` | Send notification | `message.send` |
| `message` | Send message | `message.send` |
| `clarify` | Ask user for clarification | `message.ask` |
| `memory` | Memory read/write | `memory.read`, `memory.write` |
| `image_generate` | Generate images | `media.generate`, `network.outbound` |
| `text_to_speech` | Text-to-speech synthesis | `media.generate`, `network.outbound` |

### Execution Tools (exec)

| Tool | Description | Capabilities |
|------|-------------|--------------|
| `exec` | Execute process/command | `process.exec` |
| `execute_code` | Execute code snippets | `code.exec`, `process.exec` |
| `process` | Process management (spawn/signal/stdin) | `process.exec`, `process.manage` |
| `delegate_task` | Delegate task to sub-agent | _inherits worker capabilities_ |
| `spawn_task` | Spawn background sub-agent task | _inherits worker capabilities_ |

### Dangerous Tools (dangerous)

| Tool | Description | Capabilities |
|------|-------------|--------------|
| `cronjob` | Create/manage scheduled jobs | `scheduler.write` |
| `skill_install` | Install external skill packages | `skill.install`, `network.outbound`, `fs.write` |
| `skill_manage` | Manage/delete skills | `skill.write`, `fs.write` |

---

## Risk Levels

The system defines four risk levels via the `RiskLevel` enum:

| Level | Value | Approval Requirement | Description |
|-------|-------|---------------------|-------------|
| Read-only | `read_only` | Never requires approval | Pure read operations, no side effects |
| Write | `write` | Auto-approved in interactive mode | Has side effects but protected by sandbox/path policy |
| Exec | `exec` | Requires allowlist or approval | Process/code execution, may produce arbitrary side effects |
| Dangerous | `dangerous` | Always requires human approval | Creates persistent privileged state (cron jobs/skill installs) |

Risk classification takes the stricter of two sources:

1. **Static map** (`_TOOL_RISK_MAP`): Hardcoded per tool name
2. **Tool declaration** (`Tool.risk_level`): Declared by the tool class itself

```python
# Classification logic: take the maximum severity
risk = max(static_risk, declared_risk, key=severity)
```

!!! warning "Fail-safe escalation"
    When the static map and tool declaration disagree, the system takes the stricter level. This means an MCP tool declaring `risk_level="exec"` will not be downgraded to `write` just because it is absent from the static map.

---

## Approval Flow

Tool calls pass through `ApprovalGate.check()` via a multi-step decision pipeline:

```
+-- Step 1: Static security guard (GuardDecision)
|   +-- deny -> immediate rejection (irreversible destructive commands)
|   +-- ask/allow -> continue
|
+-- Step 2: Elevated execution rights check
|   +-- security policy requires elevation but source unauthorized -> deny
|
+-- Step 3: Risk classification
|   +-- classify_risk(tool_name, args, declared_risk)
|
+-- Step 4: Unattended call routing
|   +-- unattended + READ_ONLY -> approve
|   +-- unattended + cron_authorized + WRITE/EXEC -> approve
|   +-- unattended + DANGEROUS -> deny
|   +-- unattended + other -> per unattended_policy
|
+-- Step 5: READ_ONLY / WRITE -> auto-approve
|
+-- Step 6: auto_approve allowlist -> approve
|
+-- Step 7: CLI auto-approve (interactive + non-nested + non-DANGEROUS)
|
+-- Step 8: Trusted channels (trusted_channels)
|
+-- Step 9: Persistent approval record (allowlist match)
|
+-- Step 10: Smart Approval (LLM pre-screening)
|   +-- approve -> pass
|   +-- deny -> reject
|   +-- escalate/unavailable -> continue
|
+-- Step 11: Manual approval flow
    +-- nested call -> immediate deny (worker cannot answer prompts)
    +-- send approval request, wait for human decision
```

---

## ToolExecutionContext

The framework injects a frozen context object `ToolExecutionContext` into each tool execution, enabling the tool to inspect call origin and permissions:

```python
@dataclass(frozen=True)
class ToolExecutionContext:
    execution_id: str       # Unique execution ID
    trace_id: str           # End-to-end trace ID
    session_key: str        # Session identifier (lock/history/delivery)
    memory_scope: str       # Memory scope (owner-aware)
    user_id: str            # Initiating user
    agent_id: str           # Current agent
    attempt_index: int      # Retry index
    idempotency_key: str    # Idempotency key (hash of trace+tool+index+params)
    is_replay: bool         # Whether this is a replay (skip side effects)
    parent_execution_id: str | None  # Parent execution ID (nested calls)
    credentials: dict       # Injected credentials
    approved_actions: frozenset[str]  # Set of approved actions
    approval_source: str    # "human" or "auto"
    allowed_tools: frozenset[str]    # Allowed tool subset for this execution
    channel: str            # Source channel
    chat_id: str            # Chat ID
    reply_to_id: str        # Reply target message ID
    unattended: bool        # Whether running unattended
    cron_authorized: bool   # Whether triggered by an authorized cron job
    inbound_event_id: str   # Inbound event ID
```

### Key Fields

**`approval_source`**: Distinguishes how the tool call was approved.

- `"human"`: Set only when a person explicitly approved this specific call at the prompt
- `"auto"`: All policy-based passes (allowlist, trusted channel, cli_auto_approve, etc.)

Tools can use this to decide whether to grant persistent privileges. For example, the `cronjob` tool only issues a `cron_authorized` grant when `approval_source == "human"`.

**`unattended`** and **`cron_authorized`**:

| Scenario | unattended | cron_authorized | Effect |
|----------|:----------:|:---------------:|--------|
| Interactive user session | `False` | `False` | Standard approval flow |
| Cron trigger (authorized) | `True` | `True` | WRITE/EXEC auto-approved |
| Cron trigger (unauthorized) | `True` | `False` | Per `unattended_policy` |
| Sub-agent worker | inherited | inherited | Cannot self-escalate |

!!! warning "DANGEROUS tools always denied when unattended"
    Even with `cron_authorized=True`, DANGEROUS-tier tools are still denied. This prevents unattended jobs from self-escalating (e.g. creating new cron jobs or installing skills).

---

## Configuration

### Tool Profiles

Select a preset tool set via `config.tools.profile`:

| Profile | Included Tools | Typical Use Case |
|---------|---------------|-----------------|
| `minimal` | Read-only + clarify + message + notify + todo | Pure Q&A bot |
| `messaging` | minimal + image_gen + memory + tts + vision | Multimodal chat |
| `coding` | messaging + file writes + knowledge_index + patch + task + workflow | Coding assistant |
| `full` | All tools (`*`) | Full-capability agent |

### Allow/Deny Lists

```yaml
tools:
  profile: full
  allow: []          # Override profile with explicit allowlist (profile ignored when set)
  also_allow: []     # Additional tools on top of profile
  deny: []           # Explicit blocklist (highest priority)
```

`deny` takes precedence over both `allow` and `also_allow`.

### Security Profiles

`config.security.profile` layers additional restrictions on top of tool profiles:

| Profile | Additional Blocks | Use Case |
|---------|-------------------|----------|
| `personal_cli` | No additional restrictions | Local development / personal use |
| `daemon` | Blocks exec/execute_code/process/skill_install | Background daemon |
| `public_gateway` | Blocks all write + exec + dangerous tools | Public-facing gateway |

### Network Policy

```yaml
execution:
  network_policy: deny   # deny | allow | restricted
```

When set to `deny`, `web_fetch`, `web_search`, and all tools declaring `network.outbound` capability are filtered out.

### Approval Configuration

```yaml
permissions:
  approval:
    auto_approve: [exec]          # Tools that skip approval entirely
    trusted_channels: [telegram]  # Trusted channels (EXEC may pass; DANGEROUS still needs approval)
    cli_auto_approve: true        # Auto-approve EXEC in interactive CLI mode
    mode: "smart"                 # off | smart | strict
    unattended_policy: "deny"     # deny | allow_safe
    wait_timeout_seconds: 120     # Human approval timeout
    smart_model: ""               # Model used for Smart Approval
  elevated:
    enabled: true                 # Enable the elevation mechanism
    allow_from:                   # Per-channel user elevation mapping
      telegram: [user_123]
```

`allow_from` lives under `permissions.elevated`, not under `approval`; the mapping has no effect while `elevated.enabled` is false.

!!! note "Matching rules"
    `allow_from` values are matched against the channel's `sender_id` as exact strings. `"*"` works both as a channel key (applies to every channel) and as a user value (applies to every user on that channel). Users in `permissions.admin_users` are always treated as elevated. Elevation applies only to `exec`, `execute_code` and `process`, and only when execution lands on a local/remote host or `tools.exec.security` is `full`.

### Workspace Restrictions

```yaml
tools:
  restrict_to_workspace: false   # Restrict file operations to workspace
  safe_write_root: ""            # Root directory for permitted writes (empty = unrestricted)
```

---

## Smart Approval (LLM Pre-screening)

When approval mode is `smart`, EXEC-tier tool calls are pre-screened by an LLM before falling through to human approval:

1. Tool name, arguments, and flag reason are injected into a review prompt
2. The LLM must respond with exactly `APPROVE`, `DENY`, or `ESCALATE`
3. Result handling:
   - `APPROVE` -> pass through
   - `DENY` -> reject immediately
   - `ESCALATE` / unrecognized -> fall through to human approval

---

## Code Examples

### Custom Tool

```python
from echo_agent.tools import Tool, ToolExecutionContext, ToolResult

class MyCustomTool(Tool):
    name = "my_tool"
    description = "Perform a custom operation"
    parameters = {
        "type": "object",
        "properties": {
            "input": {"type": "string", "description": "Input content"},
        },
        "required": ["input"],
    }
    timeout_seconds = 60
    max_retries = 2
    risk_level = "write"
    capabilities = ("fs.write",)

    def is_ready(self) -> bool:
        # Check external dependencies
        return True

    def readiness_detail(self) -> tuple[bool, str]:
        return True, "All dependencies ready"

    async def execute(self, params: dict, ctx: ToolExecutionContext | None = None) -> ToolResult:
        input_text = params["input"]
        # ... execution logic ...
        return ToolResult(success=True, output=f"Processed: {input_text}")
```

### Using Context for Permission Checks

```python
async def execute(self, params: dict, ctx: ToolExecutionContext | None = None) -> ToolResult:
    if ctx and ctx.unattended and "my_tool" not in ctx.approved_actions:
        return ToolResult(
            success=False,
            error="Explicit authorization required in unattended mode",
            error_kind="business",
        )

    if ctx and ctx.approval_source != "human":
        # Only perform high-risk operations with human approval
        return ToolResult(
            success=False,
            error="This operation requires human confirmation",
            error_kind="business",
        )

    # ... execution logic ...
    return ToolResult(success=True, output="ok")
```

### Tool Policy Configuration (config.yaml)

```yaml
tools:
  profile: coding
  also_allow:
    - exec
    - process
  deny:
    - skill_install
    - cronjob
  restrict_to_workspace: true
  safe_write_root: /home/user/project

permissions:
  approval:
    mode: smart
    auto_approve:
      - exec
    trusted_channels:
      - cli
    unattended_policy: deny

security:
  profile: daemon

execution:
  network_policy: allow
```

---

## Security Guards

Beyond risk levels, shell/process tools pass through pattern-matching guards:

### Hard Blocks

The following patterns trigger an immediate denial that cannot be overridden by approval:

- Root recursive deletion (`rm -rf /home`, `rm -rf /etc`)
- Block device writes (`dd of=/dev/...`)
- Filesystem formatting (`mkfs.*`)
- System shutdown (`shutdown`, `reboot`, `halt`)
- Sensitive credential path reads (`/etc/shadow`, `/root/.ssh`)

### Soft Blocks (Approval Required)

The following patterns require approval before proceeding:

- Recursive deletion (`rm -r`)
- World-writable permissions (`chmod 777`)
- Service control (`systemctl stop/restart`)
- Pipe-to-shell execution (`curl ... | bash`)
- Inline interpreters (`python -c`, `bash -c`)
- Credential file writes (redirects to `.env`, `id_rsa`, etc.)
- Destructive SQL (`DROP TABLE`, `TRUNCATE`)

!!! warning "SSRF Protection"
    The `web_fetch` tool triggers an approval prompt when targeting internal/private addresses (`127.0.0.1`, `10.*`, `192.168.*`, etc.) to prevent SSRF attacks.

---

## Capability Declarations

Each tool declares capabilities via its `capabilities` attribute or the static `TOOL_CAPABILITIES` map. Capabilities are used for:

- Tool policy filtering (`PUBLIC_GATEWAY_DENY_CAPABILITIES`, `DAEMON_DENY_CAPABILITIES`)
- Audit log classification
- MCP tools are uniformly tagged as `mcp.call`

Complete capability list:

| Capability | Meaning |
|------------|---------|
| `fs.read` | Filesystem read |
| `fs.write` | Filesystem write |
| `process.exec` | Process execution |
| `process.manage` | Process management (signal/stdin) |
| `code.exec` | Code execution |
| `network.outbound` | Outbound network requests |
| `scheduler.write` | Scheduled job creation |
| `skill.install` | Skill installation |
| `skill.write` | Skill management |
| `skill.read` | Skill viewing |
| `workflow.write` | Workflow orchestration |
| `memory.read` | Memory read |
| `memory.write` | Memory write |
| `message.send` | Message sending |
| `message.ask` | User prompting |
| `media.generate` | Media generation |
| `media.read` | Media analysis |
| `knowledge.read` | Knowledge base retrieval |
| `knowledge.write` | Knowledge base indexing |
| `session.read` | Session retrieval |
| `task.write` | Task management |
| `agent.read` | Agent listing |
| `agent.dispatch` | Agent routing |
| `mcp.call` | MCP tool invocation |
