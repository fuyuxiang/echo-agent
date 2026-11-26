# Headless Mode and Scripting

Headless mode runs EchoAgent non-interactively from the command line. It accepts a single prompt, executes it with full tool access, and returns the result. Use it to automate tasks, script workflows, build integrations, and parse output programmatically.

---

## Basic Usage

Passing a prompt non-interactively triggers headless mode. The most common way is the `-p` flag (short for `--single`); `--prompt-json` and `--prompt-file` also trigger it:

```bash
echoagent -p "Your prompt here"
```

EchoAgent processes the prompt, runs any necessary tools, and prints the result to stdout. The process exits when the response is complete.

---

## Command-Line Options

| Flag                    | Description                                           |
| ----------------------- | ----------------------------------------------------- |
| `-p, --single <PROMPT>` | The prompt to send (or use `--prompt-json` / `--prompt-file`) |
| `-m, --model <MODEL>`   | Model to use (e.g., `echo-agent-build`)              |
| `-s, --session-id <ID>` | Create a **new** session with this **UUID** (errors if invalid UUID or already in use under the target session directory; does not resume, use `-r`/`-c`) |
| `--fork-session`        | With `-r`/`-c`, fork into a new session ID instead of appending to the original |
| `-r, --resume <ID_OR_TITLE>` | Resume an existing session by ID, or by title for the current directory, ignoring letter case (a sole manually renamed match wins among duplicates; remaining duplicates error with their IDs; UUID-shaped values always take the ID path; scripts should prefer IDs) |
| `-c, --continue`        | Continue the most recent session in current directory  |
| `--cwd <PATH>`          | Set working directory                                 |
| `--output-format <FMT>` | Output format: `plain`, `json`, `streaming-json`, `streaming-messages-json` |
| `--include-partial-messages` | Emit raw `stream_event` deltas. Only affects `--output-format streaming-messages-json`; ignored (with a warning) otherwise. |
| `--yolo`                | Auto-approve all tool executions                      |
| `--rules <TEXT>`        | Custom rules for the system prompt                    |
| `--tools <TOOLS>`       | Allowlist of built-in tools (comma-separated). MCP meta-tools remain available unless denied. Headless only. |
| `--disallowed-tools <TOOLS>` | Denylist of built-in tools to remove (comma-separated). Supports `Agent` entries. Headless only. |
| `--max-turns <N>`       | Maximum number of agentic turns before stopping. Headless only. |
| `--reasoning-effort` / `--effort <LEVEL>` | Reasoning effort for reasoning models. Canonical levels: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` (each a distinct tier; a model only accepts the levels its menu advertises). Also accepts per-model menu option ids (e.g. `deep` → mapped wire value), same as `/effort`. Works in TUI and headless. |
| `--permission-mode <MODE>` | Permission mode. `bypassPermissions` enables always-approve (see [Permissions and safety](22-permissions-and-safety.md#permission-modes)); for deny-by-default use `defaultMode` in `.claude/settings.json`. |
| `--allow <RULE>`        | Permission allow rule with glob patterns (repeatable). Works in TUI and headless. |
| `--deny <RULE>`         | Permission deny rule with glob patterns (repeatable). Works in TUI and headless. |
| `--prompt-json <JSON>`  | Prompt as JSON content blocks                         |
| `--prompt-file <PATH>`  | Prompt from a file                                    |
| `--verbatim`            | Send prompt exactly as given                          |
| `--no-auto-update`      | Disable update checks for this session                |
| `--sandbox <PROFILE>`   | Sandbox profile for filesystem/network access         |

> **Note:** `--tools`, `--disallowed-tools`, `--max-turns`, and `--agents` are headless-only flags. If used in the interactive TUI, a warning is printed and the flag is ignored. `--reasoning-effort`/`--effort`, `--permission-mode`, `--allow`, and `--deny` work in both modes. For more flags (agents and worktrees), see [Additional Headless Flags](#additional-headless-flags).

### Tool Filtering

Use `--tools` to restrict the agent to an explicit set of tools (allowlist), or `--disallowed-tools` to remove specific tools from the default set (denylist). Both accept comma-separated tool names.

Tool names are internal tool IDs (e.g. the shell tool is `run_terminal_cmd`, not `bash`).

```bash
# Only allow read-only tools
echoagent -p "Explain this codebase" --tools "read_file,grep,list_dir"

# Remove web access and file editing
echoagent -p "Review this code" --disallowed-tools "web_search,web_fetch,search_replace"

# Remove shell access
echoagent -p "Review this code" --disallowed-tools "run_terminal_cmd"
```

`--disallowed-tools` also supports special `Agent` entries to control subagent spawning:

| Entry                  | Effect                                  |
| ---------------------- | --------------------------------------- |
| `Agent`                | Block all subagent spawning             |
| `Agent(explore)`       | Block the `explore` subagent type only  |
| `Agent(explore, plan)` | Block multiple specific types           |

```bash
# Prevent the agent from spawning any subagents
echoagent -p "Fix this bug" --disallowed-tools "Agent"

# Block only the explore subagent
echoagent -p "Refactor this module" --disallowed-tools "Agent(explore)"
```

`--tools` preserves the selected agent profile's injection policy: stock profiles inject enabled optional tools before applying the allowlist, while curated profiles remain strict. The final toolset retains requested tools plus always-on MCP meta-tools. When both flags are present, `--disallowed-tools` wins.

### Permission Rules (`--allow` / `--deny`)

Permission rules control whether specific tool invocations are auto-approved, denied, or require user confirmation. Unlike `--disallowed-tools` (which removes tools entirely), permission rules leave tools available but gate their execution.

Rules use `ToolPrefix(glob_pattern)` syntax:

| Prefix        | What it controls                   |
| ------------- | ---------------------------------- |
| `Bash(...)`   | Shell command execution            |
| `Edit(...)`   | File editing (path glob)           |
| `Write(...)`  | File writing (path glob)           |
| `Read(...)`   | File reading (path glob)           |
| `Grep(...)`   | Search operations (path glob)      |
| `WebFetch(...)` | URL fetching (glob or `domain:host`) |
| `MCPTool(...)` | MCP tool invocations              |

For path rules (`Read`, `Edit`, `Write`, `Grep`), `*` is a single-level wildcard and `**` is recursive. For `Bash` rules, `*` matches any characters including spaces. A bare prefix without parentheses matches all invocations of that type, and `Bash(cmd:*)` is equivalent to prefix matching on `cmd`. See [22-permissions-and-safety.md](22-permissions-and-safety.md#rule-matching-reference) for the full matching semantics.

```bash
# Deny shell commands matching "rm*"
echoagent -p "Clean up this project" --deny "Bash(rm*)"

# Allow npm commands, deny sudo
echoagent -p "Set up the project" --allow "Bash(npm*)" --deny "Bash(sudo*)"

# Allow all bash commands (auto-approve without prompting)
echoagent -p "Build the project" --allow "Bash"
```

`--allow` and `--deny` can be repeated. Deny rules take precedence over allow rules.

---

## Output Formats

Headless mode supports four output formats, selected with `--output-format`.

### plain (default)

Human-readable text, suitable for direct display or piping:

```
Here's a summary of the codebase...
```

### json

A single JSON object emitted after the response completes: response text,
stop reason, session ID, request ID (plus `thought` when reasoning is present).
When the prompt reached the model, the same object also carries spend fields
(`usage`, `num_turns`, `modelUsage`, cost). `stopReason` is the snake_case
ACP/Messages token (`end_turn`, `max_tokens`, …).

```json
{
  "text": "Here's a summary of the codebase...",
  "stopReason": "end_turn",
  "sessionId": "abc123",
  "requestId": "xyz789",
  "num_turns": 7,
  "usage": {
    "input_tokens": 7210,
    "cache_read_input_tokens": 41000,
    "cache_creation_input_tokens": 0,
    "output_tokens": 1893,
    "reasoning_tokens": 412,
    "total_tokens": 50103
  },
  "modelUsage": {
    "echo-agent-build": {
      "inputTokens": 7210,
      "outputTokens": 1893,
      "cacheReadInputTokens": 41000,
      "modelCalls": 7,
      "costUSD": 0.01268905
    }
  },
  "total_cost_usd": 0.01268905,
  "total_cost_usd_ticks": 126890500
}
```

Usage notes:

- `usage` sums tokens for the prompt, including subagents that finished
  before turn end (also under their own `modelUsage` keys). Compaction and
  other side-model calls are excluded.
- **Token field policy (headless result / `end` / error spend):**
  - `usage.input_tokens` and `modelUsage.*.inputTokens` are **uncached only**.
  - `cache_read_input_tokens` / `cacheReadInputTokens` are cache hits.
  - `total_tokens` is full input + output (includes both cache buckets):
    `total_tokens = input_tokens + cache_read_input_tokens + cache_creation_input_tokens + output_tokens`.
  - ACP `_meta.usage.inputTokens` (PromptUsage) is still the **full** prompt
    sum; only the headless projector subtracts cache. Prefer headless fields
    for spend automation.
- `num_turns` counts main-agent model rounds recorded on the prompt ledger
  (tool-loop rounds that reported usage). Subagent sampler calls do not
  increase it. Per-model call counts (including subagents) stay on
  `modelUsage.*.modelCalls`. This is the same counter family as `--max-turns`,
  not a guarantee of exact equality when rounds lack usage or hit gates.
- `total_cost_usd` appears only when the server reported a **complete** cost.
  Absence means unreported or incomplete, never free. Cost is stamped for
  API-key traffic today; pool/OAuth paths often omit it until the server
  stamps cost. When some calls lacked cost, `cost_is_partial` is true and
  **all** cost floats are omitted (`total_cost_usd` and every
  `modelUsage.*.costUSD`) so consumers cannot sum model rows into a fake
  complete bill.
- `total_cost_usd_ticks` is the same value in exact integer ticks
  (1 USD = 10^10 ticks) and appears under the same conditions. Use it for
  billing reconciliation: summing per-invocation ticks matches the server's
  usage export exactly, which float dollars cannot guarantee.
- When subagent usage could not be applied, nested subagent usage was incomplete,
  or the success-path drain timed out (up to 120s on the turn task),
  `usage_is_incomplete` is true and cost floats are omitted the same way
  (token totals may under-count subagents). Cancel snapshots without that long
  drain and marks incomplete while subagents are still live. Incomplete with
  no recorded tokens emits only `usage_is_incomplete` (no zero `usage` object).
- A prompt that never reached the model omits the spend fields.

The `sessionId` field is useful for resuming the conversation later.

On failure, EchoAgent emits an error object (process exit non-zero). Prompt-level
failures may also include frozen spend fields when usage was recorded:

```json
{"type":"error","message":"Couldn't start session: ..."}
```

### streaming-json

Newline-delimited JSON, one `type`-tagged object per line, derived from the agent's ACP session updates. Leaf field names (`toolCallId`, `kind`, `rawInput`, `rawOutput`) follow ACP; `toolName` and the `usage` line are EchoAgent additions. Consume it by switching on `type`.

```json
{"type":"thought","data":"Analyzing the directory structure..."}
{"type":"tool_call","toolCallId":"call_1","title":"Read","kind":"read","status":"in_progress","toolName":"read_file","rawInput":{"path":"src/main.rs"},"content":[],"locations":[]}
{"type":"tool_call_update","toolCallId":"call_1","status":"completed","content":[],"rawOutput":{"lines":42},"locations":[]}
{"type":"text","data":"Here's a summary"}
{"type":"usage","messageId":"resp_1","stopReason":"end_turn","usage":{"input_tokens":812,"output_tokens":45,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"reasoning_tokens":0},"signature":"..."}
{"type":"end","stopReason":"end_turn","sessionId":"abc123","requestId":"xyz789","usage":{...},"num_turns":7,"modelUsage":{...}}
```

Event types:

| Type               | Description                                                                                  |
| ------------------ | ------------------------------------------------------------------------------------------- |
| `text`             | A chunk of the agent's response text                                                          |
| `thought`          | Internal reasoning (thinking tokens)                                                          |
| `tool_call`        | A tool call the agent started (`toolCallId`, `toolName`, `kind`, `status`, `rawInput`, `content`, `locations`) |
| `tool_call_update` | Progress or result for a tool call (`status`, `rawOutput`, `content`, `locations`)            |
| `usage`            | Per-response boundary (`messageId`, `stopReason`, `usage`, `signature`), one per model response |
| `plan`             | The agent's current plan (`entries`)                                                          |
| `available_commands` | Tool and slash command lists (`tools`, `commands`)                                          |
| `end`              | Final event with metadata and spend fields when available                                    |
| `error`            | An error occurred (carries `message`, and spend fields if any)                               |

`end` is always the last event. Spend fields on `end` match the json object
shape (snake_case uncached `input_tokens`, safe cost floats). `end.stopReason`
is the turn stop reason in snake_case (`end_turn`, `max_tokens`,
`max_turn_requests`, `refusal`, `cancelled`); the verbatim per-response provider
reason (e.g. `tool_use`, `pause_turn`) is on the `usage` line's `stopReason`.
Per-response `message_id`/`stopReason`/`signature` are populated on the Messages
API backend; other backends report what they carry.

EchoAgent may also emit `max_turns_reached` and `auto_compact_*` events; treat the list as non-exhaustive and switch on `type`.

### streaming-messages-json

Newline-delimited JSON in the Messages API `stream-json` wire format. The data-bearing surface matches the Messages shape exactly. This includes the `assistant`/`user` message bodies, `usage`, `tool_use`/`tool_result`, inline web search, `stop_reason`, and the `--include-partial-messages` event framing. A consumer that reconstructs messages, reads spend, or detects errors works without changes.

The `system`/`init` and terminal `result` lines carry metadata. EchoAgent emits the fields it has real data for and omits pure-placeholder fields it cannot fill, rather than zero-filling them. As a result, those two lines may not pass strict `init`/`result` schema validation. The individual fields are listed below. Read the fidelity notes before treating any one field as authoritative. For a clean EchoAgent-native stream with no placeholder shape, use `streaming-json`.

The stream opens with a `system`/`init` line, then `assistant` messages whose `message.content[]` holds `text`, `thinking`, and `tool_use` blocks, `user` messages carrying `tool_result` blocks, and a terminal `result`:

```json
{"type":"system","subtype":"init","session_id":"abc123","apiKeySource":"user","model":"echo-agent-build","cwd":"/repo","permissionMode":"default","tools":["read_file","bash"],"slash_commands":["review"],"mcp_servers":[{"name":"linear","status":"connected"}],"skills":[],"uuid":"..."}
{"type":"assistant","message":{"id":"msg_0","type":"message","role":"assistant","model":"echo-agent-build","content":[{"type":"text","text":"Let me read the file."},{"type":"tool_use","id":"call_1","name":"read_file","input":{"path":"src/main.rs"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{...}},"parent_tool_use_id":null,"session_id":"abc123","uuid":"..."}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"fn main() {}","is_error":false}]},"parent_tool_use_id":null,"session_id":"abc123","uuid":"..."}
{"type":"result","subtype":"success","is_error":false,"duration_ms":0,"duration_api_ms":0,"num_turns":7,"result":"Here's a summary...","stop_reason":"end_turn","total_cost_usd":0.0127,"usage":{"input_tokens":812,"output_tokens":210,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"server_tool_use":{"web_search_requests":0}},"modelUsage":{},"session_id":"abc123","uuid":"..."}
```

Message types:

| Type        | Description                                                              |
| ----------- | ---------------------------------------------------------------------- |
| `system`    | Session preamble (`subtype: "init"`) with model, cwd, permission mode, tools, slash commands, and MCP servers. `subtype: "compact_boundary"` marks an auto compaction |
| `assistant` | A model message; `message.content[]` holds `text`/`thinking`/`tool_use`, plus `server_tool_use`/`web_search_tool_result` for inline backend web search |
| `user`      | Tool results, as `tool_result` blocks inside `message.content[]`         |
| `result`    | Terminal message with final text, stop reason, and spend fields         |

The `assistant` and `user` messages carry `session_id`, `uuid`, and `parent_tool_use_id` (`null` for the main conversation). The `system`/`init` and terminal `result` lines carry `session_id` and `uuid` but no `parent_tool_use_id`.

The `uuid` on each line is freshly generated per emitted line. It is not a provider, message, or event id, and not a correlation key. It does not match the provider `message.id` (that value rides `assistant.message.id`). It is unique per line, even for lines that describe the same message, and it carries no cross-line or cross-run identity. Do not use it to correlate or deduplicate.

Text and reasoning chunks are grouped into one assistant message per model response. A response's parallel `tool_result` blocks are grouped into a single `user` message. `result.result` is the final assistant message text. A model response that produces no content blocks emits no `assistant` line in the default mode. Only `--include-partial-messages` surfaces such a response, as its empty `message_start` … `message_stop` envelope.

On `init`, `skills` is live. It lists the session's user-invocable skill names, a subset of `slash_commands` sourced from the session's advertised commands, or `[]` when the session surfaces no skills. The `init` line is emitted once, deferred to the first output line so it captures the session's advertised `tools`, `slash_commands`, and `skills`. The Messages schema defines no second `init`, so a command list that changes after streaming begins is not re-advertised.

The other `init` fields carry real data:

- `apiKeySource` is `user` for API-key auth and `oauth` otherwise. EchoAgent does not distinguish the schema's `project`, `org`, and `temporary` sources.
- `permissionMode` is the effective headless mode mapped to the Messages enum: the `--permission-mode` value, or `bypassPermissions` under `--yolo`, else `default`. EchoAgent-only modes such as `auto` collapse to `default`.
- `mcp_servers[].status` reflects configuration, not live connection state. A configured server always reports `"connected"`, because per-server handshake state is not resolved by the time `init` is emitted.

EchoAgent omits the schema's pure-placeholder `init` fields it has no data for, rather than emitting dummy values: `claude_code_version`, `output_style`, and `plugins`.

`result` includes `duration_ms`, `duration_api_ms`, `num_turns`, `stop_reason`, `total_cost_usd`, `usage` (Messages API `message.usage` shape), and `modelUsage`. It also includes `errors[]` on the error subtypes. EchoAgent omits the schema's always-empty `permission_denials`, because it does not collect permission denials. `structured_output` (with `--json-schema`) is snake_case, matching the schema.

`model` appears on `init` and every `assistant` frame. It is the real model id when known, and the literal `"unknown"` only when no model is known at emit time.

The assistant frame's `stop_sequence` is wired end-to-end. It carries the provider's matched stop sequence when the model stopped on a configured one (`stop_reason: "stop_sequence"`), and is `null` on every other stop reason and backend. In `--include-partial-messages` framing, the matched sequence rides both the flushed `assistant` frame and the partial `message_delta.stop_sequence`, so a partial rebuild matches the frame. Only the partial `message_start.stop_sequence` stays `null`, because the matched sequence is not known at message open.

The emitted error subtypes are `error_max_turns`, `error_during_execution`, and `error_max_structured_output_retries`. The schema's `error_max_budget_usd` subtype is never emitted, because echoagent has no budget feature.

`result.usage` reports the Messages `message.usage` shape with the three token buckets disjoint: `input_tokens` (uncached), `cache_read_input_tokens`, and `cache_creation_input_tokens`. EchoAgent derives these from the turn's aggregate ledger, reshaped into those buckets. Subagent cache creation is included in `cache_creation_input_tokens`. The aggregate ledger tracks it as its own bucket, so it is no longer folded into `input_tokens`.

`result.usage` always emits numeric buckets, even when data is missing. This happens when the turn's usage ledger is incomplete (the same condition that surfaces `usage_is_incomplete` in the `json` format), or when no aggregate ledger reached the reducer at all. Any bucket echoagent cannot account for falls back to `0`, because the Messages API schema has no marker for incomplete or absent usage. The reducer logs a warning to stderr in both cases. Read an all-zero `usage` here as "unknown", not "free".

The nested `server_tool_use` counter is populated. `web_search_requests` is the number of *successful* backend web searches emitted this run. Failed searches and non-search `WebSearch` actions such as open_page are excluded, matching the Messages API, which does not bill errored searches. A failed backend search still emits a `web_search_tool_result` in the error shape (`content.type: "web_search_tool_result_error"`), but is not counted. Its `error_code` is a fixed `"unavailable"` placeholder, not a code forwarded from the backend. There is no `web_fetch_requests` key, because echoagent has no server-side `web_fetch`, so the placeholder is omitted.

Backend web search is inline. It folds into the same `assistant` frame as the surrounding text. The frame carries a `server_tool_use` block (`name: "web_search"`, `input.query`) immediately followed by a `web_search_tool_result` block. That result block's `tool_use_id` matches the `server_tool_use.id`, and its `content` is a `web_search_result` hit array of `{type, url, title}`. This matches the Messages API's inline server-tool shape rather than splitting the response across frames.

X search and code interpreter are a documented divergence. They stay generic, surfaced as a client `tool_use` block plus a `user` `tool_result`, because the Messages API defines no inline block type for them. Every other client tool likewise keeps the `tool_use`/`tool_result` split.

`--include-partial-messages` emits the raw event framing so a consumer can rebuild each message with the Messages streaming accumulator. The framing is `message_start`, `content_block_start`/`content_block_delta`/`content_block_stop`, `message_delta`, and `message_stop`. It carries the structural events an accumulator needs. The deltas are coarser than the Messages API's token-level streaming: tool input arrives as a single `input_json_delta`, and `citations_delta` is never produced (see below). The result is a faithful reconstruction of each message rather than a token-by-token replay.

On the Messages API backend, the framing is faithful. `message_start` carries the real provider `message.id` and the input-side `usage`. A thinking block emits its `signature_delta` in order, before the block's `content_block_stop`. The `message_start.usage` input side reports all three prompt-side buckets known at message open: `input_tokens` (the uncached portion), `cache_read_input_tokens`, and `cache_creation_input_tokens`. A cache hit is therefore visible on `message_start`, rather than only appearing later on `message_delta`/`result`. `output_tokens` seeds `0` there and is finalized on `message_delta`. A response that starts but produces no content still emits the `message_start` … `message_stop` envelope with no content blocks.

Some backends surface per-response metadata only at end of turn. Those backends fall back to a synthesized `message_start.id` and zero-seeded input `usage`. They defer the reasoning `signature` to the final `assistant` line, which is authoritative in that case.

Tool-call input is emitted as a single `input_json_delta` carrying the complete arguments JSON, followed by `content_block_stop`. It is not a sequence of token-level fragments. This is a deliberate divergence from the Messages API's incremental `partial_json` streaming. EchoAgent's ACP tool-call path delivers each tool call as one validated JSON object once the arguments are fully parsed, so a single delta is the accurate representation. A consumer that concatenates `partial_json` reassembles the identical object either way. The backend web-search `server_tool_use` block's `input.query` is emitted the same way, as one `input_json_delta`.

The Messages API `citations_delta` carries inline citations for cited text spans, such as those from web search. This stream does not produce it. EchoAgent's Messages content deltas are limited to text, thinking, signature, and tool-input JSON, so there is no citation data to surface as a `citations_delta`. Backend web-search source URLs are reported inline on the completed `web_search_tool_result` block instead (see above), not as per-span text citations.

Fidelity caveats apply to a few fields.

`duration_ms` is the prompt-execution wall clock. `duration_api_ms` is the summed *reported* per-call model time. A model call that does not report its own duration contributes `0`, so `duration_api_ms` can under-count the true API time.

`num_turns` and `total_cost_usd` are authoritative when known. When they are not, `num_turns` falls back to the count of completed model responses this turn, and `total_cost_usd` falls back to `0`. A completed but contentless response emits no `assistant` line, yet still counts as a turn. Spend is never overreported.

`modelUsage` carries the per-model token and cost fields echoagent tracks, plus `webSearchRequests` attributed to the active model. The reducer tracks a single global web-search count rather than per-model, so the whole count lands on the current or last model and other rows stay `0`. A per-model `modelUsage.*.costUSD` is `0` when that model's cost is unknown or withheld. This is the same fail-closed-to-zero behavior as the top-level `total_cost_usd`. The `json` format omits cost floats entirely when partial, but this stream keeps the field present and `0`. `contextWindow` is the current model's real total context window (the same value echoagent uses for auto-compaction), and it appears only on the current model's row. Other rows omit it, and so does the current row when the window is unknown. `maxOutputTokens` has no echoagent catalog, so that key is omitted entirely. `modelUsage` is `{}` when no per-model breakdown is available.

Like `streaming-json`, this stream is read only. Tool approvals and other bidirectional flows use the ACP interface (`echoagent agent`).

---

## Session Management in Headless Mode

By default, each `echoagent -p` invocation creates a fresh session. To maintain context across calls, use session flags.

### Named Sessions (`-s`)

To carry context across headless calls, use `-r/--resume` or `-c/--continue`. Use `-s/--session-id` only for a **new** session with a **UUID** (errors if not a UUID or already in use under the target directory). Older hidden `-s` upsert/resume behavior is gone. Use `-r`/`-c` to continue. With `-r`/`-c`, `-s` requires `--fork-session`:

```bash
# Start a headless session and capture its ID
echoagent -p "Review the changes in this PR" --output-format json | jq -r '.sessionId'

# Continue in the same session
echoagent -p "Now check for security issues" --resume "<id>"

# Optional: create with a client-chosen UUID (must not already exist)
echoagent -p "hello" --session-id "$(uuidgen | tr '[:upper:]' '[:lower:]')" --output-format json
```

> **Note:** `-s/--session-id` creates a new session only (valid UUID; errors if already in use). Use `-r` to resume.

### Resume (`-r`)

The `-r/--resume` flag resumes a specific session by ID, or by title for the current directory when the value is not an ID, ignoring letter case (a sole manually renamed match wins among duplicates; remaining duplicates error with their IDs; UUID-shaped values always take the ID path, so scripts should prefer IDs). It errors if the session does not exist:

```bash
# Get the session ID from a previous JSON response
echoagent -p "Remember: the secret number is 42" --output-format json
# Output includes "sessionId": "abc123"

# Resume that exact session
echoagent -p "What's the secret number?" --resume abc123
```

### Continue (`-c`)

The `-c/--continue` flag continues the most recent session in the current working directory:

```bash
echoagent -p "Continue where we left off" -c
```

### Extracting Session IDs

Use `--output-format json` and parse the `sessionId` field:

```bash
echoagent -p "Hello" --output-format json | jq -r '.sessionId'
```

---

## Piping Input and Output

Headless mode works naturally with Unix pipes and redirection.

### Standard Output

```bash
# Pipe output to a file
echoagent -p "Generate a README" > README.md

# Parse JSON output with jq
echoagent -p "List files" --output-format json | jq -r '.text'
```

### Standard Input

Headless mode does not read piped stdin into the prompt. Pass external content through command substitution or `--prompt-file`:

```bash
# Include git diff as context via command substitution
echoagent -p "Write a concise commit message for these changes:

$(git diff --staged)"

# Or read the prompt from a file
echoagent --prompt-file ./prompt.txt
```

---

## CI/CD Integration Examples

### Automated Code Review

```bash
echoagent -p "Review changes for bugs and security issues." \
  --output-format json --yolo | jq -r '.text' > review.md
```

### Pre-Commit Hook

```bash
echoagent -p "Review staged changes for obvious bugs. Reply OK if fine, or list issues." \
  --yolo --output-format json | jq -r '.text' | grep -q "^OK" || exit 1
```

### Batch Processing

```bash
for file in src/*.js; do
  echoagent -p "Migrate $file from CommonJS to ES modules." --yolo
done
```

---

## Scripting Patterns

### Python Wrapper

EchoAgent's headless mode can be wrapped as an OpenAI-compatible chat completion API:

```python
import asyncio
import json
import os

class EchoAgentChat:
    """Simple OpenAI-compatible wrapper using headless mode."""

    def __init__(self, cwd="."):
        self.cwd = cwd
        self.env = {**os.environ}

    def _build_cmd(self, prompt, model, stream):
        return ["echoagent", "-p", prompt, "-m", model, "--cwd", self.cwd,
                "--output-format", "streaming-json" if stream else "json",
                "--yolo"]

    async def create(self, messages, model="echo-agent-build", stream=False):
        prompt = messages[-1]["content"] if len(messages) == 1 else "\n".join(
            f"{m['role']}: {m['content']}" for m in messages
        )
        cmd = self._build_cmd(prompt, model, stream)

        if stream:
            return self._stream(cmd)

        proc = await asyncio.create_subprocess_exec(
            *cmd, env=self.env, stdout=asyncio.subprocess.PIPE
        )
        stdout, _ = await proc.communicate()
        data = json.loads(stdout.decode()) if stdout else {"text": ""}
        return {
            "choices": [{
                "message": {"role": "assistant", "content": data.get("text", "")},
                "finish_reason": "stop"
            }]
        }

    async def _stream(self, cmd):
        proc = await asyncio.create_subprocess_exec(
            *cmd, env=self.env, stdout=asyncio.subprocess.PIPE
        )
        async for line in proc.stdout:
            if not line.strip():
                continue
            event = json.loads(line)
            if event.get("type") == "text":
                yield {"choices": [{"delta": {"content": event["data"]}}]}
            elif event.get("type") == "end":
                yield {"choices": [{"delta": {}, "finish_reason": "stop"}]}


async def main():
    client = EchoAgentChat(cwd=".")
    response = await client.create(
        [{"role": "user", "content": "What files are here?"}]
    )
    print(response["choices"][0]["message"]["content"])

asyncio.run(main())
```

### Shell Script

```bash
#!/bin/bash
# Run a code review and exit with failure if issues are found

RESULT=$(echoagent -p "Review this PR for bugs. Output JSON with 'issues' array." \
  --output-format json --yolo | jq -r '.text')

ISSUE_COUNT=$(echo "$RESULT" | jq '.issues | length' 2>/dev/null || echo "0")

if [ "$ISSUE_COUNT" -gt 0 ]; then
  echo "Found $ISSUE_COUNT issues"
  echo "$RESULT" | jq '.issues[]'
  exit 1
fi

echo "No issues found"
```

---

## Always-approve for automation

`--always-approve` (alias `--yolo`, same as `--permission-mode bypassPermissions`) runs tool calls without interactive permission prompts. Deny rules, hooks, and admin locks still apply (see [Permissions and safety](22-permissions-and-safety.md#permission-modes)).

```bash
echoagent -p "Format all files" --always-approve
echoagent -p "Run the tests and fix any failures" --cwd ~/projects/my-app --always-approve
```

For agent servers and SDKs, see [Agent mode](15-agent-mode.md#automation-and-sdks).
---

## Environment Variables for Headless

Key environment variables that affect headless mode:

| Variable                        | Description                                                   |
| ------------------------------- | ------------------------------------------------------------- |
| `ECHO_AGENT_API_KEY`        | API key for authentication (required when no browser login)   |
| `ECHO_AGENT_HOME`                    | Override config directory (default: `~/.echo-agent`)                |
| `ECHO_AGENT_LOG_FILE`                | Path to a log file (used verbatim as the path; works in headless and TUI, honors `RUST_LOG`) |
| `RUST_LOG`                     | Log level filter (e.g. `debug`). Headless logs to stderr.     |

For CI environments without browser access, set `ECHO_AGENT_API_KEY` with an API key from [console.echo-agent.invalid](https://console.echo-agent.invalid):

```bash
export ECHO_AGENT_API_KEY="echo-agent-..."
echoagent -p "Run the test suite" --yolo
```

---

## Exit Codes

| Code | Meaning                              |
| ---- | ------------------------------------ |
| `0`  | Success. The prompt completed normally |
| `1`  | Error. Authentication failure, network error, or runtime error |
| `130` | Interrupted by SIGINT (Ctrl+C)                                   |
| `143` | Terminated by SIGTERM                                            |

---

## Authentication for Headless Environments

For headless use, authenticate with one of:

- **`ECHO_AGENT_API_KEY`**: simplest for CI. See [Environment Variables](#environment-variables-for-headless) above.
- **`echoagent login --device-auth`** (or `--device-code`): no browser needed on the target machine.
  See [Authentication > Device Code Flow](02-authentication.md#device-code-flow).
- **`echoagent login`**: browser-based OAuth2 on machines with a GUI.

If you've previously logged in, cached credentials are used automatically.

---

## Tips

- Headless mode starts a **fresh session by default**. Use `-r/--resume` or `-c/--continue` to maintain context across calls.
- The `--output-format json` response always includes a `sessionId` you can use with `--resume` for follow-up calls.
- Combine `--yolo` with `--rules` to set guardrails: `echoagent -p "..." --yolo --rules "Never delete files"`.
- For debugging, raise the log level and capture stderr: `RUST_LOG=debug echoagent -p "..." 2> debug.log`.

---

## Project Root Discovery

When EchoAgent starts, it discovers the project root by walking upward from `--cwd`
(or the current directory) until it finds a `.git` directory.

Note: If `--cwd` is nested inside a large repository (such as a monorepo),
EchoAgent discovers that repository as the project root and scopes its discovery (AGENTS.md, skills, git history) to it, which can make
startup slow. Point `--cwd` at the specific subproject you want to work in to keep
the scope small.

---

## File Locations

EchoAgent stores data in `~/.echo-agent` (override with `ECHO_AGENT_HOME`; see [Environment Variables for Headless](#environment-variables-for-headless)):

| Path                     | Contents                              |
| ------------------------ | ------------------------------------- |
| `config.toml`            | User configuration                    |
| `auth.json`              | Cached OAuth2/API credentials         |
| `version.json`           | Version cache for update checks       |
| `sessions/`              | Session transcripts (SQLite)          |
| `memory/`                | Cross-session memory store            |
| `logs/`                  | Internal log files (for example `unified.jsonl`) |
| `logs/mcp/`              | MCP server logs                       |
| `skills/`                | User skill definitions                |
| `personas/`              | User-scoped agent personas            |
| `crash/`                 | Crash reports                         |
| `trace-exports/`         | Session trace exports                 |
| `worktrees/`             | Git worktree metadata                 |

### Read-Only `~/.echo-agent`

For containers or CI, mount `~/.echo-agent` read-only:

- Pre-populate `auth.json` or use `ECHO_AGENT_API_KEY`
- Session persistence fails silently (ephemeral)
- Update checks log a warning and skip

```bash
export ECHO_AGENT_API_KEY="echo-agent-..."
export ECHO_AGENT_DISABLE_AUTOUPDATER=1
echoagent -p "..." --no-auto-update
```

---

## Update Check Suppression

| Method                          | Scope     |
| ------------------------------- | --------- |
| `--no-auto-update`              | Session   |
| `ECHO_AGENT_DISABLE_AUTOUPDATER=1`    | Process   |
| Non-TTY stderr (auto-detected)  | Automatic |
| `[cli] auto_update = false`     | Persistent|

`ECHO_AGENT_DISABLE_AUTOUPDATER` set to a falsy value (`0`, `false`, `off`, `no`, or empty, any
case) counts as not set. The agent SDKs
inject `ECHO_AGENT_DISABLE_AUTOUPDATER=1` for the non-leader agents they spawn (a falsy value in
the SDK's isolation env keeps updates on), and the stdio agent skips its background update
unless it runs from the managed install (`$ECHO_AGENT_HOME/bin/echoagent`).

Update messages go to **stderr**. Stdout stays clean for `--output-format json`. See also [Environment Variables for Headless](#environment-variables-for-headless).

---

## Additional Headless Flags

These flags supplement the [Command-Line Options](#command-line-options) table above. Flags already listed there (`--prompt-json`, `--prompt-file`, `--verbatim`, `--sandbox`, `--no-auto-update`) are not repeated here.

| Flag                          | Description                                       |
| ----------------------------- | ------------------------------------------------- |
| `--agent <NAME>`              | Agent name or definition file path                |
| `--agents <JSON>`             | Inline subagent definitions as JSON               |
| `--system-prompt-override`    | Override the agent's system prompt                |
| `--no-plan`                   | Disable plan mode                                 |
| `--no-subagents`              | Disable subagent spawning                         |
| `ECHO_AGENT_MEMORY=0`                | Disable cross-session memory for the process      |
| `--disable-web-search`        | Disable web search and fetch tools                |
| `--no-alt-screen`             | Run inline (no alternate screen)                  |
| `--worktree [NAME]`           | Start session in a new git worktree               |
| `--ref <REF>` / `--worktree-ref <REF>` | Branch/tag/commit to base the worktree on (with `--worktree`) |

---

## Interrupted Headless Runs

On SIGINT/SIGTERM:

- Session state saved up to the last completed tool call
- File modifications by tools are **not rolled back**
- Exit code is **130** for SIGINT (`128 + 2`) and **143** for SIGTERM (`128 + 15`); CI pipelines can distinguish these from a normal error (exit code `1`)
- Resume: `echoagent -p "continue" --resume "<id>"` or `echoagent -p "continue" --continue`

See [Session Management in Headless Mode](#session-management-in-headless-mode) for details on named sessions and the `-s`/`-r`/`-c` flags.
