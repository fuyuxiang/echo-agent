# Permissions and safety

Control what EchoAgent can access and do: permission modes, allow/ask/deny rules, hooks, and the optional OS-level sandbox.

- **Modes** set how often EchoAgent asks for approval (always-approve, auto, ask, and related).
- **Rules** set which tools are allowed, asked about, or blocked within that baseline.

---

## Permission modes

When EchoAgent edits a file, runs a command, or calls an external tool, it may pause for approval. Permission modes control how often that happens.

Modes set a baseline. Allow, ask, and deny [rules](#configuring-permissions) still apply on top of any mode.

### Starting points

| Situation | Mode |
| --------- | ---- |
| Interactive TUI | Auto for fewer prompts with background checks, or ask to approve every action yourself |
| Scripts, SDKs, CI, agent servers | Always-approve; add [deny rules](#configuring-permissions) or hooks for hard limits |

If you haven't chosen a mode, new interactive sessions use the current default. Once you pick one (via `Shift+Tab`, `/settings`, a `permission_mode` config entry, or a `--permission-mode` flag), your choice always wins and is remembered. Headless runs (`echoagent -p`), `agent stdio`, and agent servers always start in **ask**.

```bash
echoagent -p "Run the tests" --always-approve
echoagent agent --always-approve stdio
echoagent agent --always-approve serve --bind 127.0.0.1:2419 --secret <token>
```

ACP clients can set `"_meta": { "yoloMode": true }` on `session/new`. See [Agent mode](15-agent-mode.md#automation-and-sdks).

### Available modes

| Mode | What runs without asking | Best for |
| ---- | ------------------------ | -------- |
| `default` (**ask**) | Read-only tools and built-in read-only shell commands | Interactive day-to-day use |
| `acceptEdits` | File edits without a prompt | Local coding while you review diffs later |
| `plan` | Accepted for compatibility; use [plan mode](19-plan-mode.md) for gated planning | Claude-compatible settings |
| `auto` | Work the safety check allows; other calls are blocked or escalated | Interactive sessions that want fewer prompts |
| `dontAsk` | Only pre-approved tools and built-in read-only handling | Strict CI allowlists |
| `bypassPermissions` (**always-approve**) | Tool calls in general (`deny` rules, hooks, and some shell `ask` rules still apply) | Trusted automation and agent servers |

**Always-approve** is the product name; config and Claude-compatible settings may use `bypassPermissions` for the same mode. Always-approve and auto are mutually exclusive (always-approve takes precedence when both are requested).

### How to set the mode

**Interactive TUI:** `Shift+Tab` / `Ctrl+O`, `/always-approve` or `/auto`, or `/settings` ([shortcuts](03-keyboard-shortcuts.md), [commands](04-slash-commands.md)).

**CLI:**

```bash
echoagent --always-approve -p "Run the test suite"
echoagent --permission-mode auto
echoagent agent --always-approve serve --bind 127.0.0.1:2419 --secret <token>
```

**Config:**

```toml
[ui]
permission_mode = "always-approve"   # or "auto", "ask", …
```

Claude-compatible `defaultMode` in `.claude/settings.json` is also supported (see [Claude-compatible settings](#3-claude-code-compatibility-claudesettingsjson)). CLI overrides config for that process.

### Always-approve

Skips ordinary permission prompts so tools run without waiting for a click. `deny` rules, hooks, and some shell `ask` rules still apply. Admins can lock the mode off (below).

| Mechanism | Example |
| --------- | ------- |
| CLI | `--always-approve` (alias `--yolo`), or `--permission-mode bypassPermissions` |
| Config | `[ui] permission_mode = "always-approve"` |
| Interactive | `/always-approve`, `Ctrl+O` |
| ACP | `_meta.yoloMode: true` on `session/new` |

#### Always-approve with hard limits

Keep always-approve for automation, and add deny rules for paths or commands you never want run:

```toml
# project .echo-agent/config.toml
[ui]
permission_mode = "always-approve"

[permission]
deny = [
  "Bash(rm -rf *)",
  "MCPTool(sales__delete_*)",
]
```

```bash
echoagent -p "Deploy the service" --always-approve --deny 'Bash(rm -rf *)'
```

Deny always wins over allow and over always-approve’s normal pass-through. See [Configuring permissions](#configuring-permissions).

### Auto mode

Reduces interactive prompts by checking many tool calls before they run. Routine local work often proceeds; other calls may be blocked or escalated. In non-interactive sessions, a blocked call fails and is reported to the model (for example `Auto mode blocked this action …`). Behavior is the same for `echoagent -p`, `agent stdio`, and `agent serve`.

For automation that must run tools without interactive approval, use always-approve (and deny rules if you need hard blocks) rather than auto alone.

### Disable always-approve (administrators)

Organizations can prevent always-approve from being enabled via CLI, TUI, or `/always-approve`. Set this in `requirements.toml` (user-level under `~/.echo-agent/`, or system-wide under `/etc/echoagent/` for enforcement users cannot remove):

```toml
[ui]
disable_bypass_permissions_mode = true
```

Do not use `permission_mode` for this lock; that key is a switchable default. The legacy `[ui] yolo = false` key in `requirements.toml` also disables always-approve for compatibility.

EchoAgent can still load Claude-style permission **rules** from managed settings; always-approve is locked with `requirements.toml` as shown above.

---

## How a tool call is authorized

When the model requests a tool, the following checks happen in order:

1. **`PreToolUse` hooks**. A hook can deny a tool call before any other check. A hook that allows a call does not skip the checks below; it only declines to deny. See [10-hooks.md](10-hooks.md).

2. **Permission rules** (from configuration files or `--allow`/`--deny` flags)
   - A matching `deny` rule rejects the call. `deny` wins over every other rule.
   - A matching `ask` rule prompts you, including for file reads, searches, and shell commands that would otherwise be auto-approved.
   - A matching `allow` rule approves the call.

3. **Remembered grants**. Per-command approvals you saved from earlier prompts apply here, scoped to the current project. An existing grant can satisfy an `ask` rule instead of re-prompting. Commands on the [dangerous list](#dangerous-commands) prompt again rather than using a remembered prefix. See [Interactive Approvals](#interactive-approvals-and-where-they-persist).

4. **Built-in auto-approvals**. Read-only tools and a fixed set of read-only shell commands run without prompting (see below).

5. **Prompt policy** (set by the [permission mode](#permission-modes)): prompt you, auto-approve, or auto-deny the call.

[Always-approve](#always-approve) short-circuits this pipeline after step 2: `deny` rules, hooks, and `ask` rules that match a shell command's segments still apply, but remembered grants (including remembered "never allow" entries) are not consulted, and `ask` rules on non-shell tools do not prompt.

---

## Operations That Never Prompt by Default

The operations below are treated as read-only and run without prompting, in every mode including `dontAsk`, unless a matching `deny` rule or a hook blocks them. An `ask` rule forces a prompt for file reads, searches, and shell commands (see [How a Tool Call Is Authorized](#how-a-tool-call-is-authorized)).

### Read-Only Tools

- `read_file`
- `list_dir`
- `grep` (content search)
- `web_search`
- `todo_write`
- `get_command_or_subagent_output` / `wait_commands_or_subagents` / `kill_command_or_subagent` (subagent control)
- Invoking skills

### Read-Only Shell Commands

After splitting chained commands (on `&&`, `||`, `;`, and pipes), the following commands are recognized as read-only when they appear as the primary command. This list is word-boundary matched, so `ls` does not match `lsof` or `less`. (Your own `Bash(...)` rules match differently; see [Rule Matching Reference](#rule-matching-reference).)

**Filesystem (read-only viewing):**
- `ls`, `cat`, `pwd`, `date`, `whoami`, `hostname`, `uptime`, `ps`
- `head`, `tail`, `wc`, `sort`, `uniq`, `tr`, `cut`

**Git (read-only):**
- `git status`, `git branch`, `git log`, `git diff`, `git ls-files`, `git show`, `git rev-parse`
- `git blame`, `git describe`, `git merge-base`, `git shortlog`
- `git check-ignore`, `git check-attr`, `git cat-file`, `git ls-tree`, `git show-ref`, `git for-each-ref`, `git rev-list`, `git name-rev`, `git count-objects`

**Search and inspection:**
- `grep`, `rg` (not `rg --pre` / `rg --pre=…`, which spawn a preprocessor per file)

**Kubernetes (read-only):**
- `kubectl get`, `kubectl logs`, `kubectl describe`

> **Note:** `tee` is not on this list because it can write its input to arbitrary files. `cargo check` is not on this list because it compiles and runs `build.rs`, proc-macros, and any `build.rustc-wrapper` from the repo (in Ask mode it therefore prompts; Auto mode may still heuristic-allow `cargo` as a project code runner). `sort --compress-program=…` (including unique long-option abbreviations), `git -c` / `--config-env` overrides, and a git command whose local/worktree config installs an executable hook (`core.fsmonitor`, a `diff.*.command`/`textconv`/`external` driver, or a shell `alias.<safe-subcommand> = !…`) raise a request-level floor and prompt rather than auto-approve, unless the user granted that exact full script or always-approve is enabled.

These checks apply per segment. In a command like `ls && rm -rf /`, the `ls` segment is recognized as read-only, but the `rm` segment is not on the list. In `default` mode the `rm` segment prompts; under `dontAsk` it is denied.

---

---

## Configuring Permissions

EchoAgent reads permission rules from three compatible sources. Rules from all sources are merged into one set; a rule's effect depends on its action (`deny` > `ask` > `allow`), not on which file it came from.

### Where Permission Rules Live (Scopes)

Permission rules can be global (all projects), project-scoped (one repository), or personal to you within a project:

| Scope | File | Shared with teammates |
|-------|------|-----------------------|
| Global (all projects) | `~/.echo-agent/config.toml` | No |
| Project (committed) | `<project>/.echo-agent/config.toml` | Yes (commit it) |
| Project (personal) | `<project>/.claude/settings.local.json` | No (gitignore it) |
| Interactive grants | Stored internally by EchoAgent, per project | No |

Notes on scoping:

- EchoAgent discovers a `.echo-agent/config.toml` at every directory level from the repository root down to your working directory, so a subdirectory can add rules on top of the repo root's.
- Rules from all scopes are merged into one rule set; `deny` > `ask` > `allow` applies across scopes, so a global `deny` cannot be overridden by a project `allow`.
- EchoAgent has no native `config.local.toml`. For personal, uncommitted rules in a project, use `.claude/settings.local.json`; EchoAgent reads it directly (see [Claude Code Compatibility](#3-claude-code-compatibility-claudesettingsjson)).
- Interactive "Always allow" decisions are stored outside the repository, scoped to the project (see [Interactive Approvals](#interactive-approvals-and-where-they-persist)).

To stop prompts for a specific command in one project, add a narrow allow rule to that project's `.echo-agent/config.toml` (or `.claude/settings.json`):

```toml
[permission]
allow = ["Bash(cargo test *)", "Bash(npm run build)"]
```

This approves only the listed commands. Always-approve mode, by contrast, approves all tool calls.

### 1. CLI Flags

```bash
echoagent -p "Review the API changes" \
  --allow 'Bash(git *)' \
  --allow 'Bash(gh *)' \
  --allow 'Read' \
  --allow 'Grep' \
  --deny 'Bash(rm -rf *)'
```

`--allow RULE` and `--deny RULE` can be repeated and are always enforced.

Rule syntax examples:
- `Bash(git *)` — any command starting with `git `
- `Bash(npm run build)` — exact command (or prefix)
- `Bash(git commit:*)` — the `cmd:*` suffix form, equivalent to prefix matching on `git commit`
- `Read(src/**)` — read access under `src/`
- `Edit(**/*.rs)` — edit any Rust file
- `Grep` — all grep operations
- `MCPTool(my-server__*)` — MCP tools from a specific server

See [Rule Matching Reference](#rule-matching-reference) for the exact matching semantics, including how chained commands and wildcards are evaluated.

### 2. Native Configuration (`~/.echo-agent/config.toml` and `.echo-agent/config.toml`)

```toml
[permission]
rules = [
  { action = "allow", tool = "bash", pattern = "git *" },
  { action = "allow", tool = "bash", pattern = "gh *" },
  { action = "allow", tool = "read" },
  { action = "allow", tool = "grep" },
  { action = "deny",  tool = "bash", pattern = "rm -rf *" },  # block a dangerous pattern
  { action = "ask",   tool = "edit" },
]
```

The structured `tool` field accepts the lowercase names `bash`, `read`, `edit`, `grep`, `mcp`, `webfetch`, and `websearch`, corresponding to the tool classes in [Tool Names](#tool-names).

Because `deny` always wins, you cannot combine these `allow` rules with a catch-all `deny` on `bash` to mean "only allow git/gh"; a `deny tool = "bash"` rule would block `git` and `gh` too. For deny-by-default, use `defaultMode: "dontAsk"` in `.claude/settings.json` or a `PreToolUse` hook (below).

Rules from the global `~/.echo-agent/config.toml` and every project `.echo-agent/config.toml` (from the repo root down to your working directory) are merged into one rule set, alongside any `.claude/settings.json` rules.

Managed configuration deployed by your organization also contributes `[permission]` rules: the system `/etc/echoagent/managed_config.toml`, and a user-level copy that EchoAgent maintains automatically at `~/.echo-agent/managed_config.toml`. Managed rules merge like rules from any other source, with two properties specific to managed `allow` rules: your own `deny` and `ask` rules win over a managed `allow` (severity ordering), and a catch-all managed `allow` is ignored when always-approve is locked off. For rules that users cannot edit away, use the root-owned system `/etc/echoagent/requirements.toml`.

Permission rules from every source are read once, when a session starts. Changes apply to the next session.

The native `[permission]` section also accepts the compact `allow` / `deny` / `ask` string-array form, using the same rule strings as the `--allow` / `--deny` flags and `.claude/settings.json`:

```toml
[permission]
deny = [
  "Read(/Users/you/private/**)",
  "Edit(/Users/you/private/**)",
  "Bash(rm -rf *)",
]
allow = [
  "Bash(git *)",
  "Bash(gh *)",
]
```

`deny` always wins over `allow` (evaluation is `deny` > `ask` > `allow`), regardless of order or source. To block reads of paths outside your project at the OS level as well, combine deny rules with the `strict` sandbox profile (see [18-sandbox.md](18-sandbox.md)).

### 3. Claude Code Compatibility (`.claude/settings.json`)

EchoAgent reads `~/.claude/settings.json` and `~/.claude/settings.local.json`, plus the project-level `<project>/.claude/settings.json` and `settings.local.json` (walking up to the repo root). The native `.echo-agent` source for permission rules is `config.toml`, described in the section above.

Example:

```json
{
  "permissions": {
    "defaultMode": "dontAsk",
    "allow": [
      "Read",
      "Grep",
      "Bash(git *)",
      "Bash(gh *)"
    ],
    "deny": [
      "Bash(rm -rf *)"
    ]
  }
}
```

Supported `defaultMode` values include `default`, `auto`, `acceptEdits`, `bypassPermissions`, `dontAsk`, and `plan`. EchoAgent reads `defaultMode` from its canonical location under `permissions`; a top-level `defaultMode` is also accepted when the nested key is absent.

`permissions.allow`, `permissions.deny`, and `permissions.ask` entries are translated into native rules and then matched with the semantics in the [Rule Matching Reference](#rule-matching-reference). Translation notes:

- Rules for MCP tools may use either the `mcp__server__tool` form found in `.claude/settings.json` files or the native `MCPTool(server__tool)` form (see [MCP Rules](#mcp-rules)).
- Rules naming an unrecognized tool, and parameter rules such as `Agent(model:opus)`, are skipped with a warning rather than failing the load.
- `permissions.additionalDirectories` is parsed but not supported.

You can import existing Claude settings interactively with **Ctrl+I** ("Import Claude settings").

---

## Rule Matching Reference

This section defines exactly how rules are matched.

### Bash Rules

A `Bash(...)` pattern matches a command (each chained segment, for `allow` rules — see "Chained commands" below) in either of two ways:

- **Prefix**: the command starts with the pattern text, compared character for character. There is no word-boundary requirement, so `Bash(git)` matches `gitleaks` as well as `git status`. Include a trailing space and wildcard (`Bash(git *)`) to require the prefix to be a whole word.
- **Glob**: the pattern matches the whole command (or the whole segment) as a glob. `*` can appear at any position and matches any characters, including spaces and slashes, so `Bash(git * main)` matches `git checkout main`. `?` and `[...]` are also supported.

Matching is case-sensitive. Leading whitespace in the command is trimmed before matching. For `deny` and `ask` rules the raw command string is otherwise not normalized; segment-level checks additionally match normalized forms (see below).

A trailing `:*` suffix on a Bash rule is stripped to a plain prefix: `Bash(git commit:*)` becomes prefix `git commit`. Because prefixes have no word boundary, a `deny` written as `Bash(sed:*)` also blocks commands such as `sed-custom`.

**Chained commands.** EchoAgent parses each command like a shell and splits it on `&&`, `||`, `;`, `|`, and newlines. The rule actions treat segments differently:

- `deny` and `ask` rules are checked against every segment, and against the whole string. One denied segment rejects the entire command.
- `allow` rules are conjunctive: the command is auto-approved by rule only when **every** segment independently matches an allow rule. `Bash(git *)` approves `git status && git diff`, but not `git status && rm -rf /` — the `rm` segment matches no allow rule, so the command falls through to the mode's normal handling (a prompt in `default` mode; the classifier in `auto` mode, which may still approve or block it; a denial under `dontAsk`). A single allow rule can therefore never approve a chain that smuggles in an unrelated command.

> **Allow rules are not a closed allowlist.** A command that matches no allow rule is not thereby denied — it falls through to the mode. In `auto` mode the classifier can approve commands your rules never mention. For deny-by-default policies, use `dontAsk` (or always-approve plus `deny` rules for hard blocks), as described under [Configuring Permissions](#configuring-permissions).

Commands that cannot be split into simple segments (subshells, command substitution `$(...)`, backticks, background `&`, control flow) prompt as a single unit when Bash restrictions are configured.

Each segment is normalized before rules are matched. Leading environment assignments such as `RUST_LOG=debug` are stripped, and a fixed set of wrappers (`timeout`, `nice`, `ionice`, `chrt`, `stdbuf`, `env`) is peeled away, so rules match the inner command: `Bash(npm test *)` approves `RUST_LOG=debug timeout 30 npm test --workers=4`. This applies to `deny`, `ask`, and `allow` rules, remembered grants, and the read-only command list.

A few more matching details:

- Rules also apply inside a literal script passed to `bash -c`. For `allow`, every command inside that script must itself be allowed.
- Wrappers not on the list (`sudo`, `xargs`, `nohup`, …) are not peeled. Write rules that name them explicitly.
- When the parser cannot safely peel a form (for example `env -S`), the command prompts instead of matching an `allow` rule.
- Matching sees the parsed words joined by single spaces, without shell quotes. Write patterns against the unquoted command.

### Dangerous Commands

A built-in list (`rm`, `chmod`, `chown`, `chgrp`, `chattr`, `pkill`, `kill`, `killall`, `git push`) prompts even when a segment is covered by a remembered command prefix or the read-only command list. An explicit `allow` rule in configuration does approve them, and always-approve mode auto-approves them like any other command; use `deny` rules to block them unconditionally. Review rules like `Bash(rm *)` carefully before adding them as allow rules.

### Read, Edit, and Grep Rules

Path patterns are globs matched against the tool path after lexical normalization (`.`/`..` collapsed; relative paths joined with the session working directory). A `~`-prefixed tool path is matched literally — never joined with the working directory — because tools expand `~` to the home directory only after the permission check:

- `*` and `?` do not cross `/`; `**` does. `Read(src/*)` matches `src/main.rs` but not `src/nested/mod.rs`; use `Read(src/**)` for the whole tree.
- A bare filename matches only that exact string. Use `**/.env` to match `.env` at any depth.
- There are no anchor prefixes: a leading `//` or `~/` in a pattern is treated as literal glob text. Write absolute-path patterns or `**/` patterns instead.
- Because `.`/`..` are collapsed before matching, rooted patterns cannot be escaped by traversal: `Read(./**)` scopes to the working directory (bare relatives like `src/main.rs` match; `./../../etc/passwd` does not), and `Read(src/**)` stays under `src/`. Unrooted patterns (`*`, or a leading `**` as in `**/*.rs`) intentionally match at any depth, anywhere.
- `Read` rules also govern `grep` searches; `Grep(...)` rules match only grep.

`Read` and `Edit` deny rules additionally apply to file paths that shell commands touch (for example `cat` or `sed` on a denied path), including literal inline scripts passed to `bash`, `sh`, `dash`, `zsh`, or `ksh` with `-c`; that shell-level check uses the same working-directory-aware normalization (an absolute operand under the working directory also matches rooted rules like `Read(src/**)`) and also resolves symlinks. The direct `read_file`/`search_replace` tool checks do not resolve symlinks. For OS-level enforcement that covers every process, combine deny rules with the sandbox ([18-sandbox.md](18-sandbox.md)).

### MCP Rules

`MCPTool(...)` patterns match the full EchoAgent tool name in `server__tool` form, with glob support: `MCPTool(linear__*)` matches every tool from the `linear` server. EchoAgent tool names carry no `mcp__` prefix.

The `mcp__` rule spelling used in `.claude/settings.json` files is also accepted and rewritten onto the same matcher: `mcp__linear` (every tool on the `linear` server), `mcp__linear__get_issue` (one tool), `mcp__linear__*` (every tool on the server), and `mcp__*` (every MCP tool).

### WebFetch Rules

- `WebFetch(domain:example.com)` matches that host and every subdomain (`api.example.com`), case-insensitively, ignoring a leading `www.`. Wildcards are not supported inside `domain:` patterns.
- A pattern without the `domain:` prefix globs against the entire URL: `WebFetch(https://api.example.com/*)`.

### Tool Names

Recognized tool names: `Bash`, `Read`, `Edit` (and `Write`), `Grep` (and `Glob`), `MCPTool`, `WebFetch`, `WebSearch`. A bare `*` rule matches every tool. Globs are not supported in the tool-name position.

Rules naming an unrecognized tool (for example `Agent(model:opus)`) are skipped with a warning rather than failing the load.

### Evaluation Order

Rules from every source are merged into one set and evaluated by severity, not order: any matching `deny` rejects, otherwise any matching `ask` prompts, otherwise any matching `allow` approves. When no rule matches, the request falls through to the built-in auto-approvals and then the prompt policy, as described in [How a Tool Call Is Authorized](#how-a-tool-call-is-authorized).

---

## Interactive Approvals and Where They Persist

When a tool call requires approval, the permission prompt offers these choices:

- **Allow once**: approve this single invocation.
- **Reject once**: reject it, optionally with a message back to the model.
- **Enable always-approve mode**: approves all future tool calls, not just the one being prompted.
- **Allow all edits this session**: shown for file edits. This grant is held in memory only and does not survive a restart.

### Per-Command "Always Allow"

A narrower set of options remembers just the specific command, MCP tool, or web-fetch domain being prompted, for example "Always allow `cargo test`". These rows are on by default. Disable them with:

```toml
# ~/.echo-agent/config.toml
[ui]
remember_tool_approvals = false
```

Organizations can disable them via the same key in `requirements.toml` or managed configuration. With the gate enabled (the default), prompts gain:

- **`Always allow: <command>`**, which persists an allow for the command prefix.
- A matching "never allow" row, which persists a deny the same way.
- Equivalent "always allow" and "never allow" rows for MCP tools and web-fetch domains. The "never allow" row always remembers the exact tool (never a whole server) or the exact domain being prompted; a remembered deny wins over any grant, and a denied domain also covers its subdomains.

The remembered prefix is limited to a short form of the command: read-only commands persist just their listed prefix (for example `git status`, not the full argument list), and other commands persist a short leading prefix. The prompt shows exactly what will be remembered before you confirm.

Commands on the [dangerous list](#dangerous-commands) (for example `git push` and `rm`) never honor a remembered *prefix*: only an exact grant for the entire command counts, so their "Always allow" row defaults to the full command. Approving it stops prompts for that exact invocation only; any different arguments prompt again. When no rememberable grant could stop a script from prompting again — a dangerous command behind an `env` prefix, or a chain whose other steps would still need approval — the "Always allow" row is not offered at all rather than saving a rule that would not work.

### Persistence Is Per Project

Interactive grants are stored in EchoAgent's own state directory under your home directory, scoped to the git repository you launched EchoAgent in (its repository root), so a grant accepted at the repo root also applies in sessions started from a subdirectory of the same repository. Outside a git repository, grants are scoped to the launch directory, and each git worktree keeps its own grants. A grant made in one project never applies in another, grants are not written into the repository, and they are not meant to be hand-edited.

To inspect or reset a project's grants, open the `sessions` subdirectory of your EchoAgent home (the `.echo-agent` directory under your home directory, or `$ECHO_AGENT_HOME`): each project directory there (URL-encoded scope root) holds a `permission.toml` (plus per-client `permission_<client>.toml` variants) listing the remembered command prefixes, globs, MCP tools/servers, web-fetch domains, and "never allow" entries. Deleting the file resets that project's grants; the next matching tool call prompts again. Treat it as read-only state — to *add* rules, use the declarative `[permission]` configuration instead.

Interactive grants are personal, per-machine state. For an allowlist you can review in code review and share with teammates, use declarative rules in the project's `.echo-agent/config.toml` instead.

---

## Restricting Bash to Specific Commands with a Hook

A `PreToolUse` hook can enforce an allow list on the `Bash` tool that applies in every permission mode. Hooks are evaluated before the permission system; a hook deny stops the call, and a hook allow falls through to the normal permission checks (so your `deny` rules still apply).

> **Note:** Hooks fail open. If a hook script crashes, times out, or is missing, the tool call proceeds as if the hook had allowed it, and the failure is reported in the UI. A hook used as a security boundary must handle its own errors, and must account for chained commands, as the example below does. See [10-hooks.md](10-hooks.md).

### Example: Allow Only `git` and `gh`

**`~/.echo-agent/hooks/git-gh-only.json`**

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "git-gh-only.sh",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

**`~/.echo-agent/hooks/git-gh-only.sh`**

```bash
#!/bin/sh
# Allow only git and gh commands, including within chained commands.

set -eu

deny() {
  echo '{"decision": "deny", "reason": "'"$1"'"}'
  exit 2
}

INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.toolInput.command // empty')

[ -n "$CMD" ] || deny "Empty command is not allowed"

# Normalize '&&' and '||' to ';' so chains can be checked segment by
# segment, then reject constructs this script cannot inspect.
CMD=$(echo "$CMD" | sed 's/&&/;/g; s/||/;/g')
case "$CMD" in
  *'$('*|*'`'*|*'&'*|*'>'*|*'<'*) deny "Substitution, background, and redirection are not permitted" ;;
esac

# Split on the separators and require every segment to start with git or gh.
echo "$CMD" | tr ';|' '\n\n' | while IFS= read -r SEGMENT; do
  SEGMENT=$(echo "$SEGMENT" | sed 's/^[[:space:]]*//')
  [ -n "$SEGMENT" ] || continue
  case "$SEGMENT" in
    git\ *|git|gh\ *|gh) ;;
    *) deny "Only git and gh commands are permitted. Blocked segment: $SEGMENT" ;;
  esac
done
```

```bash
chmod +x ~/.echo-agent/hooks/git-gh-only.sh
```

This hook denies every `Bash` command unless each chained segment starts with `git` or `gh`, and rejects command substitution, backgrounding, and redirection outright because it cannot verify what they execute. It works in every permission mode.

For hook installation, the JSON format, the trust model for project hooks, and other events, see [10-hooks.md](10-hooks.md), which also contains a complementary "block dangerous patterns" example.

---

## Example Configurations

### Headless git and gh Only (CI and Automation)

```bash
echoagent -p "Implement the feature using only git and GitHub CLI" \
  --allow 'Read' \
  --allow 'Grep' \
  --allow 'Bash(git *)' \
  --allow 'Bash(gh *)'
```

Install the `git-gh-only` hook above to deny every other `Bash` command. For deny-by-default on all tools, also set `{"permissions": {"defaultMode": "dontAsk"}}` in `.claude/settings.json`.

### Read-Only Code Reviewer

```toml
# .echo-agent/config.toml
[permission]
rules = [
  { action = "allow", tool = "read" },
  { action = "allow", tool = "grep" },
  { action = "deny",  tool = "edit" },
  { action = "deny",  tool = "bash" },
]
```

### Interactive Development

Use `default` mode plus narrow `Bash(...)` allow rules for the commands you run most (`git`, `cargo test`, `rg`, and similar).

---

## Combining with the Sandbox

Permissions control what the model is allowed to request. The OS-level sandbox (see [18-sandbox.md](18-sandbox.md)) controls what the process can do even after a command is approved.

Recommended combination for untrusted code:

1. `dontAsk` plus narrow allow rules, or a restrictive hook
2. `--sandbox strict` or a custom profile
3. Project trust plus review of any `SessionStart` hooks

---

## Managing Permissions in the TUI

- Permission decisions appear in the transcript.
- The `/always-approve` command toggles always-approve mode; other modes are set through `defaultMode` (see [How to set the mode](#how-to-set-the-mode)).
- Permission prompts include per-command "Always allow" options that persist for the current project only (on by default; disable with `[ui] remember_tool_approvals = false`). See [Interactive Approvals](#interactive-approvals-and-where-they-persist).
- To manage hooks and plugins, run `/hooks` or `/plugins` (on most terminals, **Ctrl+L** also opens the Extensions modal; on VS Code, Cursor, Windsurf, and Zed, `Ctrl+L` is mid-turn interject instead). See [10-hooks.md](10-hooks.md).

---

## Best Practices

1. **Prefer narrow patterns.** `Bash(git *)` grants less access than a bare `Bash` allow rule.
2. **Combine layers.** `dontAsk`, narrow allow rules, a restrictive hook, and the sandbox each restrict independently.
3. **Review project configuration from unfamiliar sources.** Project permission rules in `.echo-agent/config.toml` and `.claude/settings.json` are gated on folder trust: an untrusted checkout's project rules (including `allow` rules and `defaultMode`) are skipped, and their presence triggers the folder-trust question. Trusting the folder applies them, so review them — and any project hooks — before granting trust to an unfamiliar checkout (see the security notes in [10-hooks.md](10-hooks.md)).
4. **Test your policy.** With `defaultMode: "dontAsk"` set (or your `PreToolUse` hook installed), run representative commands and confirm what is blocked.
5. **Treat the read-only command list as a convenience, not a security boundary.**

---

## See also

- [Hooks](10-hooks.md) — PreToolUse and other lifecycle scripts
- [Headless mode](14-headless-mode.md) — One-shot CLI and automation flags
- [Agent mode](15-agent-mode.md) — ACP, stdio, and agent servers
- [Sandbox](18-sandbox.md) — OS-level isolation profiles
- [Configuration](05-configuration.md) — Native `config.toml` structure

