# Tasks & Planning

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Echo Agent provides full task lifecycle management: from lightweight Todo lists to DAG-driven multi-step Workflows, to multi-agent delegation via the delegate tool. All tasks are unified on the Dashboard Kanban board with real-time monitoring and manual intervention support.

---

## Core Concepts

| Concept | Tool | Purpose |
|---------|------|---------|
| **Todo** | `todo` | Lightweight task list, per-session storage, ideal for planning-phase brainstorming |
| **Task** | `task` | Persistent task records with full state machine, priority, and Kanban tracking |
| **Workflow** | `workflow` | DAG-based multi-step orchestration with automatic dependency resolution |
| **Delegate** | `delegate_task` | Multi-agent dispatch — the main Agent splits sub-tasks for parallel worker execution |

---

## Task State Machine

The core of Task management is a strict finite state machine. All transitions are validated.

### State Definitions

| Status | Meaning | Terminal |
|--------|---------|----------|
| `PENDING` | Created, awaiting queue entry | |
| `QUEUED` | Queued, awaiting execution | |
| `RUNNING` | Currently executing | |
| `BLOCKED` | Blocked by external dependency | |
| `REVIEW` | Execution complete, awaiting review | |
| `SUSPENDED` | Manually suspended | |
| `SUCCESS` | Completed successfully | Yes |
| `FAILED` | Execution failed | Yes |
| `CANCELLED` | Cancelled | Yes |

### State Transition Diagram

```
PENDING ──→ QUEUED ──→ RUNNING ──→ REVIEW ──→ SUCCESS
  │            │          │  │  │               │
  │            │          │  │  └→ SUSPENDED    │
  │            │          │  └──→ BLOCKED       │
  │            │          └────→ FAILED         │
  │            │                    │           │
  │            ←────────────────────┘ (retry)   │
  │            ←────────────────────────────────┘ (reject → re-queue)
  │            ←──── BLOCKED (unblock)
  │            ←──── SUSPENDED (resume)
  └──→ CANCELLED ← (reachable from any non-terminal state)
```

### Valid Transitions Table

```python
VALID_TASK_TRANSITIONS = {
    PENDING:   {QUEUED, CANCELLED},
    QUEUED:    {RUNNING, CANCELLED},
    RUNNING:   {REVIEW, BLOCKED, FAILED, SUSPENDED, CANCELLED},
    BLOCKED:   {QUEUED, RUNNING, CANCELLED},
    REVIEW:    {SUCCESS, QUEUED, CANCELLED},
    SUSPENDED: {QUEUED, RUNNING, CANCELLED},
    FAILED:    {QUEUED},         # retry
    SUCCESS:   {},               # terminal
    CANCELLED: {},               # terminal
}
```

!!! warning "State Transition Rules"
    - Any transition not listed above raises a `ValueError` — TaskManager rejects the operation.
    - When the Agent calls `task complete`, the tool internally performs the two-step `RUNNING → REVIEW → SUCCESS` transition automatically; no manual REVIEW step is needed.
    - `FAILED → QUEUED` is the only retry path. Each retry increments `retry_count`; once `max_retries` is exceeded, retry is no longer allowed.

---

## Task Lifecycle

### Creating a Task

```json
{"tool": "task", "action": "create", "title": "Crawl documentation", "priority": 3}
```

New tasks start in `PENDING` status with an auto-generated unique ID (format: `t_xxxxxxxxxxxx`).

### Starting and Executing

```json
{"tool": "task", "action": "start", "task_id": "t_abc123"}
```

Status flows through `PENDING → QUEUED → RUNNING`. The Agent performs actual work while in `RUNNING`.

### Completing or Failing

```json
{"tool": "task", "action": "complete", "task_id": "t_abc123", "result": "Crawled 42 documents"}
{"tool": "task", "action": "fail", "task_id": "t_abc123", "error": "Target site returned 503"}
```

### Retrying

```json
{"tool": "task", "action": "retry", "task_id": "t_abc123"}
```

Re-queues a `FAILED` task, incrementing `retry_count` by 1.

### Blocked and Suspended

A task asking for clarification becomes `BLOCKED`, preserving its session and plan without advancing the workflow. Open the task details on the board, select an option or enter an answer, then choose **Submit and continue**. The same task is queued to resume. Failed submissions keep the input. Duplicate answers, superseded questions and cancelled tasks cannot restart execution. Answering requires administrator access and grants no dangerous-tool permissions.

Tasks linked to an IM conversation continue in that chat. If a question was not delivered, recover it through **Channels → Delivery recovery** first. Background board tasks store their final response in **Result**, without requiring a chat transport.

The endpoint is `POST /api/v1/tasks/{id}/clarification` with `question_id` and a nonempty `answer` of up to 10,000 characters, under the configured gateway prefix.

- **BLOCKED**: Task cannot proceed due to an external dependency. Set `blocked_reason` to explain why.
- **SUSPENDED**: Manually paused. Can be resumed to `QUEUED` or `RUNNING` at any time.

---

## TaskRecord Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Task ID, auto-generated |
| `workflow_id` | string | Parent workflow (empty for standalone tasks) |
| `parent_task_id` | string | Parent task ID (for hierarchical decomposition) |
| `board_id` | string | Kanban board ID, defaults to `"default"` |
| `title` | string | Task title |
| `description` | string | Detailed description |
| `status` | TaskStatus | Current state |
| `priority` | int (0-9) | Priority, 0 is highest, default 5 |
| `labels` | list[str] | Label list |
| `assignee` | string | Assigned person/Agent |
| `source` | string | Source identifier |
| `session_id` | string | Associated session |
| `blocked_reason` | string | Reason for blocking |
| `review_summary` | string | Review summary |
| `result` | string | Execution result |
| `error` | string | Error message |
| `retry_count` | int | Number of retries attempted |
| `max_retries` | int | Maximum retry count, default 3 |
| `metadata` | dict | Custom metadata (workflow step info, etc.) |

---

## Workflow Orchestration

Workflow is a DAG (Directed Acyclic Graph) based multi-step orchestration engine. Each step defines its dependencies; the engine automatically resolves topological order and schedules ready steps.

### Workflow State Machine

```
PENDING ──→ RUNNING ──→ SUCCESS
  │            │  │
  │            │  └→ WAITING (paused, awaiting)
  │            │  └→ BLOCKED
  │            └──→ FAILED (terminal; inspect results before creating new work)
  └──→ CANCELLED ← (reachable from any non-terminal state)
```

### Creating a Workflow

```json
{
  "tool": "workflow",
  "action": "create",
  "name": "Data processing pipeline",
  "steps": [
    {"id": "fetch", "name": "Fetch data", "tool_name": "web_fetch", "tool_params": {"url": "https://example.com"}},
    {"id": "parse", "name": "Parse data", "tool_name": "knowledge_index", "tool_params": {"action": "status"}, "depends_on": ["fetch"]},
    {"id": "store", "name": "Store results", "tool_name": "write_file", "tool_params": {"path": "report.txt", "content": "Replace with the verified result"}, "depends_on": ["parse"]}
  ]
}
```

### StepDefinition Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Step ID (auto-generated as `step_0`, `step_1`... if not specified) |
| `name` | string | Step name |
| `tool_name` | string | Tool to execute |
| `tool_params` | dict | Tool parameters |
| `depends_on` | list[str] | List of dependency step IDs |
| `condition` | string | Reserved; only an empty string is accepted. Nonempty conditions fail validation. |
| `retry_max` | int | Nonnegative automatic retry count after a step fails; default 0. Enable only where repeating side effects is safe. |
| `timeout_seconds` | int | Positive timeout from Runtime dispatch, including queue/approval waits; default 300 seconds. |

Creation and startup reject empty graphs, duplicate step IDs, missing dependencies, and cycles. Concurrent advancement creates each step once. A PENDING step left by a crash between task creation and workflow-map persistence is adopted on the next start/advance.

### Execution Model

The Workflow engine **only orchestrates** — it resolves dependencies, creates step tasks, and advances state, but **does not execute tools**. Actual execution is performed by the Agent:

1. `workflow start` → engine creates `QUEUED` tasks for steps with no dependencies
2. Runtime TaskDispatcher claims ready tasks in isolated `task:<id>` sessions (`metadata` contains `tool_name`/`tool_params`)
3. The Agent calls tools and closes the task with `task complete` / `task fail`; Runtime also writes the actual turn outcome as a safety net
4. `task complete` automatically triggers `workflow.advance()`, engine checks dependencies and schedules subsequent steps
5. Only all-success graphs become `SUCCESS`. A failure becomes `FAILED` after its explicit retry budget is exhausted; a cancelled step makes the workflow `CANCELLED`.

Pause prevents unclaimed and dependent steps from starting; already claimed steps may finish. Resume reconciles their actual outcomes. Timeouts fail the step and cooperatively interrupt its exact turn; an executing tool may stop only at its next checkpoint, and prior side effects are not rolled back. Late results from an old attempt cannot finish a newer retry.

!!! note "advance is best-effort, not part of the transaction"
    `task complete`, `task fail`, and `task cancel` trigger one advance *after* state has been persisted.

    Persisting the task state and advancing the workflow are two separate steps, and the first completes first. A failed advance only logs a warning — it does not fail the tool call and does not roll back the task state. The cost is that DAG progress stalls where it is until the next explicit `workflow advance`. There is no background compensation job covering this.

    So a workflow that has not progressed for a while is worth checking with `workflow status`: a task in a terminal state while the workflow still sits on an older step means an advance was dropped.

### Workflow Operations

| Operation | Description |
|-----------|-------------|
| `start` | Start the workflow, schedule ready steps |
| `status` | View workflow current state and step progress |
| `advance` | Manual advance (normal flow auto-advances; use this for manual recovery) |
| `pause` | Pause workflow (status → WAITING) |
| `resume` | Resume workflow, re-schedule ready steps |
| `cancel` | Cancel workflow and all its incomplete step tasks |
| `list` | List all workflows, optionally filter by status |

---

## Todo Tool

`todo` is a lightweight task list tool with data stored per-session in local JSON files. Ideal for the Agent to organize thoughts and break down steps during the planning phase.

### Operations

```json
{"tool": "todo", "action": "create", "title": "Research competitor APIs"}
{"tool": "todo", "action": "create", "items": [
  {"title": "Step 1: Read configuration"},
  {"title": "Step 2: Validate parameters"},
  {"title": "Step 3: Execute migration"}
]}
{"tool": "todo", "action": "list"}
{"tool": "todo", "action": "update", "task_id": "t_abc123", "status": "in_progress"}
{"tool": "todo", "action": "complete", "task_id": "t_abc123"}
{"tool": "todo", "action": "delete", "task_id": "t_abc123"}
```

### Todo vs Task

| Dimension | Todo | Task |
|-----------|------|------|
| Persistence | Local JSON, session-scoped | Database-persisted |
| State machine | `pending` / `in_progress` / `done` | 9-state strict state machine |
| Kanban | Not shown on Kanban | Displayed on Dashboard Kanban |
| Workflow | Not supported | Supported as workflow step |
| Use case | Planning, thinking, temporary lists | Formal task tracking and scheduling |

---

## Multi-Agent Delegation

The `delegate_task` tool allows the main Agent (orchestrator) to dispatch sub-tasks to Worker Agents for parallel execution.

### Invocation

```json
{
  "tool": "delegate_task",
  "goal": "Translate README.md at the project root into Japanese, preserving formatting"
}
```

### Execution Model

1. Main Agent calls `delegate_task`, describing the sub-task and context
2. System creates a Worker Agent with a restricted tool set (excludes `delegate_task`/`spawn_task`/`clarify`, etc.)
3. Worker executes independently, bounded by `max_iterations` and `timeout_seconds`
4. Worker returns structured results to the main Agent
5. Main Agent synthesizes Worker results and continues

### Security Constraints

- Workers cannot call `delegate_task` (prevents recursive dispatch storms)
- Workers cannot message the user directly (`message`/`notify` are blocked)
- Worker tool calls are subject to the same approval policies as the main Agent
- Each Worker has an independent `trace_id` for audit tracking

!!! info "Risk Level"
    The `delegate_task` tool has `risk_level` set to `exec`, meaning it performs actual operations. In channels requiring approval, the dispatch itself must pass through the approval gate first.

---

## Kanban Board

### Dashboard Kanban Page

The Dashboard provides a visual Kanban board with tasks organized by status columns:

```
┌─────────┬─────────┬─────────┬─────────┬─────────┬─────────┐
│ PENDING │ QUEUED  │ RUNNING │ REVIEW  │ BLOCKED │ FAILED  │
├─────────┼─────────┼─────────┼─────────┼─────────┼─────────┤
│ Card    │ Card    │ Card    │ Card    │         │ Card    │
│ Card    │         │ Card    │         │         │         │
└─────────┴─────────┴─────────┴─────────┴─────────┴─────────┘
```

### Board Features

- **Persistent columns**: PENDING, QUEUED, RUNNING, REVIEW, BLOCKED, FAILED are always visible
- **Terminal archiving**: SUCCESS and CANCELLED tasks are hidden from the main view (toggle to view)
- **Card information**: Title, priority, labels, assignee, source, blocked reason
- **Status colors**: Each status has a distinct visual color scheme
- **Drag-and-drop**: Supports dragging cards to change status within valid transitions
- **Board isolation**: Multiple boards via `board_id`, default board ID is `"default"`

### API Access

```
GET  /api/v1/tasks?board_id=default
GET  /api/v1/tasks?status=running
POST /api/v1/tasks/{id}/transition  {"status": "queued"}
```

---

## Priority & Configuration

### Priority System

Priority ranges from 0-9, lower numbers indicate higher priority:

| Range | Meaning | Use Case |
|-------|---------|----------|
| 0-2 | Urgent | Direct user commands, blocking issues |
| 3-4 | High | Important features, time-sensitive tasks |
| 5 | Default | Regular tasks |
| 6-7 | Low | Optimizations, non-urgent improvements |
| 8-9 | Lowest | Background cleanup, experimental tasks |

### Concurrency & Retry

- `max_retries`: Maximum retry count, default 3
- `retry_count`: Number of retries attempted, incremented on each `retry` operation
- Optimistic locking (`version` field + CAS): Prevents concurrent update conflicts
- Lease mechanism (`lease_until_ms`): Prevents tasks stuck in RUNNING permanently after Worker crashes

---

## Usage Examples

### Long Conversations and Clarification

Compression replaces history only after receiving a complete, usable summary. Model failure, output truncation, content filtering, or a failure cooldown preserves the original history and media references. A head/tail transcript with no middle summary is never saved as successful compression. If the retained history still exceeds the model window, the request may report a context limit; retry after restoring the model or explicitly start a new session.

You can answer a CLI attach clarification immediately. Answers received while the prompt is still being delivered remain available until the tool consumes them. Disconnecting or cancelling the session still follows the existing interruption flow.

### Example 1: Simple Task Tracking

```
User: Fix this bug for me
Agent:
  → task create "Fix login page 500 error" priority=2
  → task start
  → (performs the fix)
  → task complete result="Fixed null pointer exception, added tests"
```

### Example 2: Multi-Step Pipeline

```
Agent:
  → workflow create "Deploy pipeline" steps=[
      {id: "test", tool_name: "exec", tool_params: {command: "pytest"}},
      {id: "build", tool_name: "exec", tool_params: {command: "docker build"}, depends_on: ["test"]},
      {id: "deploy", tool_name: "exec", tool_params: {command: "kubectl apply"}, depends_on: ["build"]}
    ]
  → workflow start
  → (engine auto-schedules test → build → deploy)
```

### Example 3: Parallel Delegation

```
Agent:
  → delegate_task goal="Translate CHANGELOG into Japanese"
  → delegate_task goal="Translate CHANGELOG into Korean"
  → (two Workers execute in parallel, main Agent synthesizes results)
```

### Example 4: Planning Phase with Todo

```
Agent:
  → todo create items=[
      {title: "Analyze existing data model"},
      {title: "Design new API interfaces"},
      {title: "Write migration script"},
      {title: "Write test cases"}
    ]
  → (execute step by step, marking complete)
  → todo complete task_id="t_xxx"
```
