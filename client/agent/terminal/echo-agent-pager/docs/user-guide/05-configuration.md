# Configuration

EchoAgent reads settings from config files, environment variables, and CLI flags. This page covers the common options. The field list for `config.toml`, `managed_config.toml`, and `requirements.toml` is [26-config-reference.md](26-config-reference.md) (extracted to `~/.echo-agent/docs/user-guide/` on launch).

---

## Precedence

Settings resolve highest-priority first:

1. **CLI flags** (e.g. `--yolo`, `--model`, `--sandbox`)
2. **Environment variables** (e.g. `ECHO_AGENT_API_KEY`, `ECHO_AGENT_MEMORY`)
3. **`requirements.toml` / MDM** (org-enforced; clamps every config layer below, including the overlay)
4. **`ECHO_AGENT_CONFIG` / `ECHO_AGENT_CONFIG_PATH` overlay** (above `config.toml` and managed, below `requirements.toml` / MDM)
5. **config.toml** (`~/.echo-agent/config.toml`)
6. **`managed_config.toml`** (org-deployed defaults; below `config.toml`)
7. **Built-in defaults**

Within the config-file tier, the layers merge lowest-to-highest: `managed_config.toml` → `config.toml` → `ECHO_AGENT_CONFIG` overlay → `requirements.toml` / MDM. So `requirements.toml` and MDM clamp **both** your `config.toml` and the overlay.

`ECHO_AGENT_CONFIG` / `ECHO_AGENT_CONFIG_PATH` (tier 4) are config **overlays**: a merged config layer, not direct-setting environment variables like `ECHO_AGENT_API_KEY` (tier 2). They set config keys (subject to the allowlist below), so read them as part of the config-file tier rather than the env-var tier.

### Injecting config with `ECHO_AGENT_CONFIG`

A harness or ACP client that launches `echoagent agent stdio` can inject settings without writing a `config.toml` or relocating `$ECHO_AGENT_HOME`:

- **`ECHO_AGENT_CONFIG`**: an inline JSON object overlay.
- **`ECHO_AGENT_CONFIG_PATH`**: an *additional* file overlay (not a replacement for `config.toml`), a JSON or TOML file read by its extension (`.json` → JSON, else TOML). `ECHO_AGENT_CONFIG` wins if both are set. An empty `ECHO_AGENT_CONFIG` is treated as unset, and a malformed one logs a warning and falls through to `ECHO_AGENT_CONFIG_PATH`.

The overlay is **deep-merged** on top of your `config.toml` (it overrides only the keys it sets), placed above the user/managed layers but **below** `requirements.toml` / MDM so an enterprise pin still wins. A malformed blob is ignored with a warning. This mirrors `CODEX_CONFIG` from the `codex-acp` adapter (a JSON object merged into the session config); EchoAgent is ACP-native, so the overlay lives in the agent itself. It only affects settings read from the merged config, and it is **not** a permission-escalation path. The overlay is confined, fail-closed, to an **allowlist** of soft settings (`models`, `features`, a narrowed `toolset`, and a `shell_environment_policy` limited to its filter fields, which select among env names the launcher already controls and cannot inject an env value into tool subprocesses); every other table is dropped at the choke point, so the overlay cannot spawn commands, set auth policy, redirect network traffic, elevate trust, or add a discovery source. Even on the allowlisted settings, a specific set of security gates read the raw disk layers rather than the overlay. The `ConfigLayers::env_overlay` rustdoc is the canonical list of what the overlay can and cannot reach and which gates read it overlay-free; see also the [internal environment-variables reference](../internal/22-environment-variables.md). Use `ECHO_AGENT_DEFAULT_SELECTED_PERMISSION` for headless permission control. For example, to set the default reasoning effort:

```bash
ECHO_AGENT_CONFIG='{"models": {"default_reasoning_effort": "high"}}' echoagent agent stdio
```

---

## config.toml (main configuration)

Location: `~/.echo-agent/config.toml`. If the file is missing, EchoAgent uses its built-in defaults, so you only need to set the values you want to override.

### General settings

```toml
[cli]
auto_update = true                     # check for updates on launch

[models]
default = "echo-agent-4.5"                   # model used for new sessions
web_search = "echo-agent-4.5"                # model used by the web_search tool

# Defaults applied to every model; a per-model [model.<id>] value always wins.
# See "Custom Models" for the per-model overrides and full details.
extra_headers = { "X-Request-Tags" = "team=example,env=prod" }
temperature = 0.7
top_p = 0.95
max_completion_tokens = 8192
max_retries = 8
inference_idle_timeout_secs = 600
subagent_rate_limit_max_attempts = 8
stream_tool_calls = true

[ui]
simple_mode = true                     # readline-style prompt editing (default); false = vim editing in the prompt
vim_mode = false                       # vim-style scrollback navigation keys (default: false)
max_thoughts_width = 120               # max column width for reasoning display
default_selected_permission = "always_allow_all_sessions" # preselected row on the FIRST approval prompt
remember_tool_approvals = true         # show per-command "Always allow" options on permission prompts;
                                       # grants are remembered per project (default: true); see 22-permissions-and-safety.md
show_thinking_blocks = true            # show agent thinking blocks in the TUI (default: true)
group_tool_verbs = true                # fold runs of read/search/list tool calls and subagent rows
                                       # — and finished thoughts among them — into one row (default: true)
collapsed_edit_blocks = false          # show edits as one-line +N/-M diffstat summaries and merge
                                       # back-to-back same-file edits into one row, expand for the
                                       # diffs (default: false; pager.toml [scrollback.blocks.edit]
                                       # expanded_by_default/line_summary override its fold shape)
page_flip_on_send = true               # pin a just-sent prompt at the top of the viewport so the
                                       # response starts on a fresh page (default: true); set false
                                       # so sending never moves the scroll position
follow_up_behavior = "queue"           # mid-turn follow-ups: "queue" (wait for turn end; default) or
                                       # "steer" (plain Enter still queues visibly, then injects at the
                                       # next tool/model safe gap). See Keyboard Shortcuts → Mid-turn.
screen_mode = "fullscreen"             # default render mode: "fullscreen" | "minimal"
                                       # (unset → fullscreen); set via /settings → Default screen mode

[features]
telemetry = false                      # anonymous usage telemetry
feedback = true                        # feedback system (default: true)
lsp_tools = false                      # expose the lsp tool
codebase_indexing = true               # code graph indexing (default: true)
two_pass_compaction = false            # prefire two-pass compaction (default: false, opt-in)
remote_fetch = true                    # allow optional online model-catalog fetches (default: true;
                                       # set false for firewalled/air-gapped deployments; background
                                       # managed-config sync has its own switch: managed_config)

[session]
auto_compact_threshold_percent = 85    # auto-compact at this % of context window (default: 85)
load_envrc = true                      # load .envrc environment variables

[tools]
respect_gitignore = false              # default: false; set true to make every tool skip gitignored files

# Optional caps on parallel media generation in a single model step.
# Per tool name. First 2×-or-more burst: discard that step and retry once.
# Any other over-cap (including a second 2× burst) keeps the first K.
# Defaults: image 8, video 4.
# Env vars ECHO_AGENT_MAX_PARALLEL_IMAGE_GEN_CALLS / ECHO_AGENT_MAX_PARALLEL_VIDEO_GEN_CALLS
# override these values (see environment-variables doc).
# [tools.media_gen]
# max_parallel_image_gen_calls = 8
# max_parallel_video_gen_calls = 4
```

#### Input mode

`[ui] simple_mode` controls how you edit text in the **prompt** — the input editor. It has nothing to do with how you move around the scrollback; that's [`vim_mode`](#vim-mode).

| Value | Behavior |
|-------|----------|
| `true` (default) | **Readline editing.** Plain readline-style text entry. |
| `false` | **Vim editing (experimental).** Vim-style modal editing (normal and insert modes). When the prompt is empty it starts in normal mode with focus on the scrollback. |

To switch the prompt to vim-style editing:

```toml
[ui]
simple_mode = false
```

You can also flip it from the settings pane (`/settings` → **Disable vim input mode**); EchoAgent writes your choice to `[ui] simple_mode`. `simple_mode` and `vim_mode` are independent — one governs the prompt editor, the other governs scrollback navigation. See [Keyboard Shortcuts](03-keyboard-shortcuts.md) for the full binding reference.

#### Default selected permission

When the agent asks to run a command (or take some other tool action), the approval menu highlights one row by default. `[ui] default_selected_permission` sets which row that is on the **first** prompt of a session.

| Value | Preselected row |
|-------|-----------------|
| `always_allow_all_sessions` (default) | The "Always allow on all sessions" row. |
| `allow_command_always` | The "Always allow this command" row. |
| `allow_once` | The "Yes" / allow-once row. |
| `reject` | The reject row. |

```toml
[ui]
default_selected_permission = "allow_once"
```

After you answer the first prompt the cursor turns **sticky**: each later prompt preselects whatever you last confirmed (pick "No" once and subsequent prompts start on their reject row), carrying across edit / bash / MCP prompts until you restart. So this setting only picks the starting point.

Values match case-insensitively; an unset or unrecognized value falls back to `always_allow_all_sessions`. The `allow_command_always` row is always scoped to the specific action being approved (command / tool / domain / edit-session), never a global allow-everything — that's what `always_allow_all_sessions` is for. Note the per-command "Always allow" rows appear while `[ui] remember_tool_approvals` is enabled (the default; set it to `false` to hide them). See [22-permissions-and-safety.md](22-permissions-and-safety.md).

You can also override this with `ECHO_AGENT_DEFAULT_SELECTED_PERMISSION`, which is handy for headless or agent test runs that shouldn't mutate `config.toml`. Precedence: env var → `config.toml` → `always_allow_all_sessions`.

#### Vim mode

`[ui] vim_mode` controls whether vim-style bindings are active in the **scrollback** pane. It does not affect the prompt.

| Value | Behavior |
|-------|----------|
| `false` (default) | Bare-letter and `Shift+letter` keys (`j`/`k`, `h`/`l`, `g`/`G`, `y`/`Y`, `o`/`O`, `r`, `x`, `e`/`E`, `H`/`L`, plus `i`) are suppressed in the scrollback: pressing one focuses the prompt and types the character. Arrows, `Tab`, `Space`, `PageUp`/`PageDown`, and every `Ctrl+letter` shortcut still navigate. `Esc` is **not** a scrollback key — it cancels a running turn, and while idle follows the clear / rewind policy (see [Keyboard Shortcuts](03-keyboard-shortcuts.md#escape)). |
| `true` | All vim-style scrollback bindings are active, exactly as listed in [Keyboard Shortcuts](03-keyboard-shortcuts.md). Mid-turn `Esc` is swallowed in this mode (`Ctrl+C` cancels); minimal mode keeps Esc-cancel regardless. |

Toggle it at runtime with `/vim-mode`, or from `/settings` → **Vim scrollback navigation**. EchoAgent writes the change to `[ui] vim_mode` immediately and applies it to every future pager session, including new agents and subagents in the same process. There's no per-session override — `config.toml` is the source of truth on next launch. `vim_mode` is independent of `simple_mode`.

#### Screen mode

`[ui] screen_mode` is the **default render mode** for plain `echoagent` launches. Set it from `/settings` → **Default screen mode** (restart required) or edit `config.toml` by hand — both write the file. CLI flags (`--minimal` / `--fullscreen`) and slash commands (`/minimal` / `/fullscreen`) are session-scoped and do **not** write this key; after a slash switch, the reverse command returns you for that session only.

| Value | Behavior |
|-------|----------|
| unset | Settings shows **Fullscreen**. There's no sticky preference at startup: legacy `pager.toml` `[terminal] minimal` can still force minimal, and terminals that leak mouse reports (JediTerm/Windows) may auto-open minimal until you set an explicit value. Otherwise the alt-screen policy picks fullscreen vs inline. |
| `"fullscreen"` | Sticky non-minimal. Fullscreen-vs-inline still follows the alt-screen policy (`--no-alt-screen`, `[terminal] alt_screen`, terminal auto-detection). |
| `"minimal"` | Sticky minimal (scrollback-native) mode. |

A CLI flag always wins over the config value for that invocation.

#### Snap prompt to top on send

By default, sending a prompt scrolls it to the top of the viewport so the response starts on a fresh page. Set `[ui] page_flip_on_send = false` (or toggle **Snap prompt to top on send** in `/settings` → Appearance) to leave the scroll position alone when you send. It takes effect on the next send — no restart.

#### Scrolling

Four `[ui]` settings tune mouse-wheel and trackpad scrolling. All apply immediately and are editable from the settings pane (`/settings` → **Scroll speed** / **Scroll input** / **Scroll lines** / **Invert scroll**).

| Key | Values (default) | Behavior |
|-----|------------------|----------|
| `scroll_speed` | `1`–`100` (`50`) | Speed multiplier for wheel and trackpad. `50` = 1.0x, `1` = 0.1x, `100` = 6.0x. |
| `scroll_mode` | `auto` \| `wheel` \| `trackpad` (`auto`) | Wheel-vs-trackpad detection is heuristic (terminal scroll events carry no magnitude); force one when auto-detection misreads your device — e.g. a wheel notch that jumps too far, or a trackpad that feels stepped. |
| `scroll_lines` | `1`–`10` (unset) | Lines per scroll tick, applied to **both** wheel and trackpad. While unset, each terminal's own profile applies (e.g. a conservative 1 line/event under tmux). Committing any value — even `3`, the number the settings pane shows — switches permanently to that explicit override. |
| `invert_scroll` | `false` \| `true` (`false`) | Reverse vertical scroll direction ("natural" scrolling). |

```toml
[ui]
scroll_speed = 50
scroll_mode = "auto"     # auto | wheel | trackpad
invert_scroll = false
# scroll_lines is unset by default: the per-terminal profile stays in charge.
# scroll_lines = 3
```

Each setting also has an environment-variable override, applied on first load only (again, handy for headless / test runs): `ECHO_AGENT_SCROLL_SPEED`, `ECHO_AGENT_SCROLL_MODE`, `ECHO_AGENT_INVERT_SCROLL` (`1`/`true`/`0`/`false`), and `ECHO_AGENT_SCROLL_LINES`. Precedence: env var → `config.toml` → default. Unrecognized values fall back to the default, and out-of-range numbers clamp.

### Tool configuration

```toml
[toolset.bash]
timeout_secs = 120.0                   # foreground command timeout in seconds (default: 120)
output_byte_limit = 20000              # max captured output in bytes (default: 20000)

[toolset.ask_user_question]
timeout_enabled = true                 # false = wait forever for answers (default: true)
timeout_secs = 1800                    # seconds to wait when enabled (default: 1800 / 30 min)

[toolset.web_fetch]
proxy_endpoint = "https://proxy.example.com"   # egress proxy URL
allowed_domains = ["docs.rs", "echo.agent"]          # override the built-in allowlist
allow_local = false                            # true = allow localhost / 127.0.0.0/8 / ::1 only

[toolset.web_search]
# Restrict web_search to these domains (max 5). Mutually exclusive with excluded_domains.
allowed_domains = ["docs.echo-agent.invalid", "arxiv.org"]
# ...or block these domains instead (leave allowed_domains unset):
# excluded_domains = ["reddit.com", "pinterest.com"]
```

`allow_local` is off by default (SSRF fail-closed). Turn it on (or set `ECHO_AGENT_WEB_FETCH_ALLOW_LOCAL=1`) and `web_fetch` may reach **explicit** loopback hosts only — private, link-local, and cloud-metadata ranges stay blocked. Resolution: TOML > env > default off.

`[toolset.web_search]` constrains the `web_search` tool's domains — the allowlist/blocklist the search itself runs under (not a post-filter). `allowed_domains` and `excluded_domains` are **mutually exclusive**; if you set both, the allowlist wins and the blocklist is dropped with a warning. An empty or absent list is unbounded. This applies to both the backend-hosted search (models with server-side search) and the client-side fallback. A configured policy is **authoritative**: it cannot be bypassed by the model — the model's own per-call `allowed_domains` is ignored whenever you have set `allowed_domains` or `excluded_domains` here (so a blocklist is a real block). The model's per-call allowlist only applies when you have configured nothing. Resolution: requirements → user `config.toml` → managed → default (unset). Config is read at session start, so edit it before starting a session — changes don't apply mid-session.

`[toolset.ask_user_question]` is honored across **requirements.toml**, **managed config**, and your user **`config.toml`**. Precedence: requirements → env (`ECHO_AGENT_ASK_USER_QUESTION_TIMEOUT_ENABLED` / `ECHO_AGENT_ASK_USER_QUESTION_TIMEOUT_SECS`) → user config → managed → defaults. Set `timeout_enabled = false` in your user config to disable the automatic questionnaire timeout for yourself; `timeout_secs` must be a positive integer. You can also toggle `timeout_enabled` from `/settings` → **Ask-Question timeout** (under Agent & Approval); changes apply to newly started sessions.

### Authentication

See [Authentication](02-authentication.md) for the full story.

```toml
[auth]
auth_provider_command = "/usr/local/bin/my-auth-provider"
auth_provider_label = "Acme Corp"
auth_token_ttl = 3600

[echo_agent_com_config.oidc]
issuer = "https://acme.okta.com"
client_id = "0oa1b2c3d4e5f6g7h8i9"
# scopes = ["openid", "profile", "email", "offline_access", "api:access"]
# audience = "https://api.acme.com"
```

### Custom models

Add custom model endpoints to use alternative providers or self-hosted models.

```toml
[model.my-model]
model = "model-id"                    # model identifier sent to API
base_url = "https://api.example.com/v1"  # OpenAI-compatible endpoint
name = "Display Name"                 # shown in model picker
description = "Model description"      # optional
api_key = "sk-..."                    # API key for this provider
env_key = "ECHO_AGENT_API_KEY"               # env var(s) holding the API key; string or array (first set, non-empty wins)
temperature = 0.7                     # sampling temperature (0.0-2.0)
top_p = 0.95                          # nucleus sampling parameter
max_completion_tokens = 8192          # max tokens per response
context_window = 128000               # context window size (for auto-compact)
query_params = { api-version = "2026-07-22" } # query params appended to every request URL
env_http_headers = { "X-Tenant" = "TENANT_TOKEN" }    # request headers from env vars, resolved at client build
```

Credential resolution: `api_key` > `env_key` > signed-in session token > `ECHO_AGENT_API_KEY`. See [Custom Models](11-custom-models.md#request-query-parameters) for `query_params` and `env_http_headers`, and [Sandbox Mode](18-sandbox.md#shell-environment-policy) for `[shell_environment_policy]`, which restricts the environment variables tool subprocesses inherit.

To override a built-in model, use its name as the section key and set only the fields you need:

```toml
[model.echo-agent-build]
api_key = "my-api-key"
```

### MCP servers

Configure external tool integrations over the Model Context Protocol.

```toml
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxx" }
enabled = true                        # enable/disable (default: true)
startup_timeout_sec = 30              # init timeout in seconds (default: 30)
tool_timeout_sec = 6000              # tool call timeout in seconds (default: 6000)
tool_timeouts = { create_issue = 120 }  # per-tool timeout overrides

[mcp_servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user:pass@localhost/db"]

[mcp_servers.my-streamable-server]
url = "https://mcp.example.com/api/mcp"  # HTTP/SSE transport
headers = { "x-mcp-session-id" = "{{session_id}}" }
```

Remote (HTTP/SSE) servers receive a default `User-Agent: echo-agent-cli/<version>` header; a
valid `User-Agent` entry in `headers` overrides it (Figma servers receive bare
`echo-agent-cli`). See [MCP servers](07-mcp-servers.md) for details.

MCP servers can also be set per-project in `.echo-agent/config.toml`. Project-scoped config contributes `[mcp_servers]`, `[plugins]`, and `[permission]` rules; every other section loads only from `~/.echo-agent/config.toml`.

Priority for `[mcp_servers]` and `[plugins]`: `.echo-agent/config.toml` (current dir) > `<repo-root>/.echo-agent/config.toml` > `~/.echo-agent/config.toml`. `[permission]` rules aren't overridden by priority — they merge across all files with `deny` > `ask` > `allow` (see [22-permissions-and-safety.md](22-permissions-and-safety.md)).

### Memory

Persist knowledge across sessions. Enable memory with `ECHO_AGENT_MEMORY=1`, `[memory] enabled = true`, or managed remote settings.

```toml
[memory]
enabled = false                       # enable memory

[memory.session]
save_on_end = true                    # write metadata summary on session end

[memory.watcher]
enabled = true                        # watch memory files for external edits

[memory.search]
max_results = 6                       # default number of results
min_score = 0.7                       # minimum relevance score

[memory.initial_injection]
enabled = true                        # auto-inject memory on first turn
min_score = 0.9                       # score threshold for first-turn injection

[memory.embedding]
# model is unset by default, so retrieval uses full-text search only
dimensions = 1024                     # vector dimensions
```

### Subagents

```toml
[subagents]
enabled = true
sampling_limit = 12                   # concurrent in-flight subagent sampling calls per process; defaults to max_concurrent (32) when unset (ECHO_AGENT_SUBAGENT_SAMPLING_LIMIT)

[subagents.toggle]
explore = true                        # enable/disable specific types
plan = false

[subagents.models]
explore = "echo-agent-build"               # route to different models
```

To pin the model a subagent uses, set its entry under `[subagents.models]`.

### Goal mode and background workflows

`/goal` has two drivers, chosen by the background-workflows setting. With workflows enabled, the host-owned workflow engine evaluates rounds and drives completion verification; with them disabled, `/goal` falls back to the legacy model-facing `update_goal` tool. Whether `/goal` is available at all is a separate switch (the goal feature setting).

Background workflows — the `workflow` tool, named `.echo-agent/workflows/*.rhai` scripts, `/deep-research`, and `/workflow` launches — are **on by default**. Disable with config, env, or remote settings.

```toml
[workflows]
enabled = false                       # disable background workflows (or ECHO_AGENT_WORKFLOWS=0)
```

Project workflows are discovered from `<repo-root>/.echo-agent/workflows/`; user workflows from `~/.echo-agent/workflows/`. Discovery and invocation key off the script's `meta.name`, so keep each filename aligned with its `meta.name`. Built-ins win over project names, and project names win over user names, so keep names unique across scopes.

Each launch gets a session-unique display handle such as `deep-research-2`. That handle is what you see in the `/workflow runs` dashboard and pass to `/workflow pause`, `resume`, or `stop` — the internal run IDs never surface in commands. A numbered handle isn't a reusable definition name, so the dashboard disables **save** until you pick a new unique `meta.name` and save the edited script yourself. See [Slash Commands](04-slash-commands.md) for examples.

### Skills

```toml
[skills]
paths = ["~/my-team-skills"]          # additional directories to scan
ignore = ["~/my-team-skills/wip"]     # paths to exclude
disabled = ["wip-skill"]              # skill names to keep listed but inactive
```

### Harness compatibility

Control vendor compatibility for Cursor, Claude, and Codex. Every cell defaults to `true`. Session cells stay staged and inert until a foreign-session scanner consumes them, and each tool needs both its `sessions` cell and the matching `resume-claude`, `resume-codex`, or `resume-cursor` skill — a missing skill means zero foreign-session filesystem I/O.

```toml
[compat.cursor]
skills = true     # scan ~/.cursor/skills/ and <cwd>/.cursor/skills/
rules = true      # scan ~/.cursor/rules/ and <dir>/.cursor/rules/
agents = true     # scan ~/.cursor/ for named instruction files
mcps = true       # scan ~/.cursor/mcp.json and <cwd>/.cursor/mcp.json
hooks = true      # scan ~/.cursor/hooks.json and <cwd>/.cursor/hooks.json
sessions = true   # staged; no scanner consumer yet

[compat.claude]
skills = true     # scan ~/.claude/skills/ and <cwd>/.claude/skills/
rules = true      # scan ~/.claude/rules/ and <dir>/.claude/rules/
agents = true     # scan ~/.claude/ and <dir>/.claude/CLAUDE*.md
mcps = true       # scan ~/.claude.json for MCP servers
hooks = true      # scan ~/.claude/settings.json for hooks
sessions = true   # staged; no scanner consumer yet

[compat.codex]
sessions = true   # staged; no scanner consumer yet
```

Codex's `skills`, `rules`, `agents`, `mcps`, and `hooks` cells are reserved and currently inert — they do not enable `.codex` discovery.

For Claude and Cursor, `rules` and `agents` are independent: turning off named instruction files doesn't disable the home or project rules directory, and turning off rules doesn't disable named files. Claude's `agents` cell gates home-level `~/.claude/` named files and project `<dir>/.claude/CLAUDE*.md`; generic top-level `Claude.md`, `CLAUDE.md`, and `CLAUDE.local.md` stay recognized. Project rule paths are scanned at every directory from the repo root down to the current one.

Each cell can be set via environment variable or `config.toml`; see the environment-variables reference for the names. Resolution: env var > config.toml > default (on).

`echoagent inspect` reports cells that still need session-start resolution as `?` until a value is available; cells with an explicit env or TOML value use that value. Affected discovery entries report `compatibilityStatus: "unresolved"` in JSON and `[compat unresolved]` in human output.

### Plugins

```toml
[plugins]
paths = ["~/my-plugins/custom-tools"]
disabled = ["user/a1b2c3d4/noisy-plugin"]
```

### Hints

`[hints]` holds small persisted UI preferences: remembered answers and modal layout. EchoAgent writes these for you as you use the TUI, but you can edit or delete them by hand; removing a key restores the default.

`[hints]` is read from the **effective config merge**, with the usual precedence: system managed → user `managed_config.toml` → user `config.toml` → user `requirements.toml` → system `requirements.toml`, higher layers winning. The TUI only ever **writes** these to your user `~/.echo-agent/config.toml`.

```toml
[hints]
memory_modal_fullscreen = false        # remember the memory modal fullscreen state
new_session_worktree_mode = "never"    # /new worktree prompt: "ask" | "always" | "never"
fork_worktree_mode = "ask"             # /fork worktree prompt: "ask" | "always" | "never"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `memory_modal_fullscreen` | bool | `false` | Remembers whether the memory modal was last opened fullscreen. |
| `new_session_worktree_mode` | string | `"never"` | Worktree prompt for `/new`: `ask` shows the popup, `always` creates a worktree, `never` skips it. |
| `fork_worktree_mode` | string | `"ask"` | Worktree prompt for `/fork`: `ask`, `always`, or `never`. |

### Notifications

Fire terminal notifications when the agent finishes a turn or needs approval. They use terminal-native protocols (OSC 9, OSC 99, OSC 777, or BEL) and are focus-gated by default, so they only fire when you're not looking at the terminal.

```toml
[ui.notifications]
method = "auto"           # auto|osc9|osc99|osc777|bel|none
condition = "unfocused"   # unfocused|always|never
idle_threshold_secs = 3   # seconds unfocused before a notification fires
events = ["turn_complete", "approval_required"]
sleep_prevention = true   # prevent display sleep during agent turns
progress_bar = true       # show tab progress bar (OSC 9;4)

[ui.notifications.title]
enabled = true
items = ["action-required", "spinner", "activity", "session-name", "echoagent"]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `method` | string | `"auto"` | Notification protocol. `auto` picks the best for your terminal. |
| `condition` | string | `"unfocused"` | When to notify: `unfocused` (only when the terminal lost focus), `always`, or `never`. |
| `idle_threshold_secs` | integer | `3` | Minimum seconds unfocused before a notification fires. |
| `events` | array | `["turn_complete", "approval_required"]` | Events that trigger notifications. Options: `turn_complete`, `approval_required`, `session_ready`, `task_complete`, `agent_error`. |
| `sleep_prevention` | bool | `true` | Keep the display awake while the agent works (macOS/Linux). |
| `progress_bar` | bool | `true` | Show a progress indicator in the terminal tab (OSC 9;4). |
| `title.enabled` | bool | `true` | Set the terminal title to reflect agent state. |
| `title.items` | array | (see above) | Items shown in the title bar. Options: `action-required`, `spinner`, `activity`, `session-name`, `cwd`, `model`, `turn-timer`, `echoagent`. |

#### Terminal support matrix

| Terminal | Auto Protocol | Focus Tracking | Progress Bar |
|----------|---------------|----------------|--------------|
| iTerm2 | OSC 9 | Yes | Yes |
| Kitty | OSC 99 | Yes | No |
| Ghostty | OSC 777 | Yes | Yes |
| WezTerm | OSC 9 | Yes | Yes |
| Warp | OSC 9 | Yes | No |
| Alacritty | BEL | Yes | No |
| VS Code | BEL | Yes | No |
| Apple Terminal | BEL | No | No |
| VTE (GNOME Terminal) | OSC 777 | Yes | No |
| EchoAgent Desktop | None (native) | N/A | N/A |
| Unknown | BEL | No | No |

With `method = "auto"`, EchoAgent detects the terminal brand and picks the best protocol. Set `method` explicitly to override that.

#### Notification hooks

Run your own commands when events fire. Hooks receive `$ECHO_AGENT_EVENT`, `$ECHO_AGENT_MESSAGE`, and `$ECHO_AGENT_SESSION_ID` in the environment.

```toml
# macOS native notification
[[ui.notifications.hooks]]
command = "terminal-notifier -title 'EchoAgent' -message '$ECHO_AGENT_MESSAGE'"
events = ["turn_complete", "approval_required"]
only_unfocused = true
timeout_secs = 10

# Push to ntfy server
[[ui.notifications.hooks]]
command = "curl -s -d '$ECHO_AGENT_MESSAGE' ntfy.sh/my-echo-agent-alerts"
events = ["turn_complete"]
only_unfocused = true
timeout_secs = 10

# Play a sound
[[ui.notifications.hooks]]
command = "afplay /System/Library/Sounds/Glass.aiff"
events = ["turn_complete"]
only_unfocused = true
timeout_secs = 5
```

| Hook Option | Type | Default | Description |
|-------------|------|---------|-------------|
| `command` | string | (required) | Shell command to run. |
| `events` | array | `[]` | Events that trigger this hook (empty = all events). |
| `only_unfocused` | bool | `true` | Only fire when the terminal has lost focus. |
| `timeout_secs` | integer | `10` | Kill the hook process after this many seconds. |

#### Troubleshooting

Run `/doctor` in the affected session. It shows the detected notification and focus issues, the relevant configuration file, and the steps to resolve them. An explicit `method = "bel"` is treated as intentional. `method = "none"` turns off notification and focus findings.

**Sleep prevention not taking effect:** on macOS, sleep prevention uses `IOPMAssertionCreateWithName` via CoreFoundation; on Linux, `systemd-inhibit` (which must be on `$PATH`). Make sure the relevant tool is available. Prevention is only active during agent turns and releases automatically when the turn ends.

### Status line

An optional row at the bottom of the full-screen pager, disabled by default. Opt in with `[ui.status_line]`:

```toml
[ui.status_line]
type = "builtin"                # builtin | command | disabled
items = ["cwd", "model", "context"]
```

The other keys are `items` (which built-in segments to show, in order), `command`, `padding`, and `refresh_interval` (in seconds; re-runs a `command` row on a timer, so an incident page or a CI status reaches an idle session). The [Status Line guide](25-status-line.md) documents all of them, along with the JSON contract a `command` script reads on stdin and an example script.

Minimal mode has no status-line row; it uses the terminal tab title instead (see [Notifications](#notifications) `title.items`).

### Keyboard shortcuts

Keyboard shortcuts are **not** configurable — all bindings are built in. See [Keyboard Shortcuts](03-keyboard-shortcuts.md) for the complete reference.

### Telemetry

These are independent knobs (see [Monitoring Usage](24-monitoring-usage.md#related-settings)):

- **`[features] telemetry`** / `ECHO_AGENT_TELEMETRY_ENABLED` — the product-analytics master switch. `/privacy` doesn't change it.
- **Coding data, retention, and training** — the Settings row `/privacy` opens; coding-data sharing, separate from telemetry.
- **`[telemetry] trace_upload`** / `ECHO_AGENT_TELEMETRY_TRACE_UPLOAD` — session traces; follows telemetry when unset.
- **`[telemetry] otel_*`** / `ECHO_AGENT_EXTERNAL_OTEL` — external OTEL to your own collector (below).

When telemetry is on, enterprises running their own collector can redirect it or turn parts off under `[telemetry]`:

```toml
[telemetry]
events_url = "https://telemetry.your-company.com/events"  # send events to your own collector
events_api_key = "your-collector-token"                   # auth for your collector, if required
mixpanel_enabled = false                                  # disable Mixpanel product analytics
trace_upload = false                                      # disable session/trace uploads (inherits the telemetry toggle when unset)
```

Set these only to point telemetry at your own infrastructure or to switch parts off. The built-in endpoint and credentials are managed by EchoAgent — leave them unset to use the defaults.

The same `[telemetry]` table also configures the **external OpenTelemetry stream**, an independent opt-in (it doesn't require the telemetry toggle above) that ships a curated, content-free usage schema to your *own* OTLP collector. Collector auth comes from `OTEL_EXPORTER_OTLP_HEADERS` and is never stored on disk. See [Monitoring & Usage](24-monitoring-usage.md) for the full schema, env vars, and privacy model.

```toml
[telemetry]
otel_enabled = true                                       # external OTEL master switch (= ECHO_AGENT_EXTERNAL_OTEL)
otel_metrics_exporter = "otlp"                            # otlp | console | none
otel_logs_exporter = "otlp"                               # otlp | console | none
otel_endpoint = "https://collector.corp.example:4318"     # OTLP base endpoint
otel_protocol = "http/protobuf"                           # http/protobuf | grpc
otel_certificate = "/etc/ssl/corp-ca.pem"                 # optional: trust private CA (path only)
otel_client_certificate = "/etc/ssl/client.crt"           # optional: mTLS client cert (path only)
otel_client_key = "/etc/ssl/client.key"                   # optional: mTLS client key (path only)
otel_log_user_prompts = false                             # content gate (admins can pin via requirements)
otel_log_tool_details = false                             # content gate (admins can pin via requirements)
```

### Version pinning

Control which versions the CLI may auto-update to and which versions may run. Set
these in `[cli]`, or in a managed layer for fleet-wide policy. Each has an
environment override that can only tighten the bound, for CI and testing.

> **Changed:** `minimum_version` no longer blocks startup. It is now a soft
> anti-downgrade floor for the updater. For a hard floor that prevents old
> versions from starting, use `required_minimum_version`.

```toml
[cli]
minimum_version = "0.2.109"          # updater won't downgrade below this
maximum_version = "0.2.180"          # updater won't install above this
required_minimum_version = "0.2.100" # refuse to start below this
required_maximum_version = "0.2.200" # refuse to start above this
```

- `minimum_version` (`ECHO_AGENT_MINIMUM_VERSION`) is a soft anti-downgrade floor. The
  updater skips a target below it and keeps the current version. It never blocks
  startup.
- `maximum_version` (`ECHO_AGENT_MAXIMUM_VERSION`) is a soft ceiling. The updater caps
  its target at it and never installs above it.
- `required_minimum_version` (`ECHO_AGENT_REQUIRED_MINIMUM_VERSION`) and
  `required_maximum_version` (`ECHO_AGENT_REQUIRED_MAXIMUM_VERSION`) are hard bounds. If
  the running version is outside the range, the CLI exits at startup and instructs
  the user to install an approved version. `echoagent update` and `echoagent --version` keep
  working so an out-of-range install can recover.
- Bounds resolve across config layers by tightening only: a floor takes the
  highest value and a ceiling the lowest, so a managed bound can't be loosened,
  and a user or environment bound can't cancel a managed hard bound. An invalid
  value is ignored so a bad policy can't block startup.
- An explicit `echoagent update --version X` is allowed above the ceiling, to recover
  from a too-new install, and rejected below the hard floor.

### Enterprise deployment

A complete config for enterprise use:

```toml
[cli]
auto_update = false

[auth]
auth_provider_command = "/usr/local/bin/my-company-auth-provider"
auth_provider_label = "Acme Corp"
auth_token_ttl = 3600

[models]
default = "company-echo-agent"

[model.company-echo-agent]
model = "echo-agent-build"
base_url = "https://echo-agent-proxy.acme.com/"
name = "EchoAgent Build Latest (Proxy)"
context_window = 128000

[features]
telemetry = false
```

---

## pager.toml (appearance configuration)

Location: `~/.echo-agent/pager.toml`. This controls the TUI's look and feel. Changes apply on restart.

### Terminal

```toml
[terminal]
alt_screen = "auto"                   # fullscreen mode: "auto", "always", "never"
```

- `auto` (default): use the alternate screen when the terminal supports it.
- `always`: always use the alternate screen.
- `never`: run inline in the terminal's main scrollback buffer.

### Animation

```toml
[animation]
fps = 30                              # animation frame rate (ticks per second)
wave_rows = 32                        # rows per wave cycle for accent animation
```

### Prompt

```toml
[prompt]
collapse_unfocused = true             # collapse prompt when scrollback is focused
mouse_hover = true                    # show hover highlight on the prompt widget
show_prefix = true                    # show the prompt prefix character
```

Compact mode isn't persisted here — control it at runtime with `[ui] compact_mode` or the `/compact-mode` command.

### Scrollback

```toml
[scrollback.layout]
outer_vpad = 1                        # vertical padding
outer_hpad_left = 2                   # left horizontal padding
outer_hpad_right = 2                  # right horizontal padding
block_pad_left = 2                    # padding inside block, left of content
block_pad_right = 2                   # padding inside block, right of content

[scrollback.scrollbar]
enabled = true                        # show scrollbar
gap_left = 0                          # gap between content and scrollbar
gap_right = 0                         # gap between scrollbar and screen edge

[scrollback.scroll]
margin = 0                            # minimum context lines above/below selection
min_page_fraction = 0                 # minimum scroll as % of viewport (0-100)
follow_indicator = "center"           # ▼/▲ scroll indicators: "center" or "none"
follow_auto_select = true             # auto-select latest entry in follow mode
follow_by_overscroll = true           # scrolling past bottom engages follow mode
anchor_on_fold = true                 # keep block position when folding
respect_manual_folds = true           # opt-in (default: false): keep manually folded blocks as-is during streaming/finish; expanding while following stops auto-scroll

[scrollback.display]
sticky_headers = true                 # pin user prompts as sticky headers
tab_width = 4                         # spaces per tab character
expandable_indicator = true           # show expand indicator on foldable entries
expandable_indicator_running = true   # show indicator on running entries
expandable_indicator_char = "›"       # character for the expand indicator (default: "›")
selection_buttons = false             # show copy/view buttons on selection
line_under_last_entry = false         # horizontal line below last entry
group_selection_split = true          # split selection box for expanded blocks
highlight_overlays_border = false     # highlight extends over selection box border
dim_accent = 0.5                      # dimming factor for collapsed accents (0.0-1.0)
```

`respect_manual_folds` is off by default. Turn it on and a block you fold by hand is pinned: streaming updates and finish events (a thinking block ending, say) leave its fold state alone, and expanding a block while follow-mode is tailing new content stops the auto-scroll so the view stays put. Follow resumes via `Shift+G`, `j` at the last entry, scrolling past the bottom, or sending a new prompt. `Shift+E` clears all pins; `Ctrl+E` clears pins on thinking blocks.

### Block configuration

```toml
[scrollback.blocks.edit]
indent = true                         # indent diff content
vpad = false                          # vertical padding
# expanded_by_default = true          # unset: follows [ui] collapsed_edit_blocks in config.toml
                                      # (flag on = collapsed one-liner); uncomment to pin either shape
dual_line_numbers = false             # two-column line numbers (old + new)
# line_summary = false                # show +N/-M in the collapsed header; unset follows the same flag
hunk_separator = "…"                  # separator between diff hunks (default: "…")

[scrollback.blocks.prompt]
vpad = true                           # vertical padding
show_prefix = true                    # show prompt prefix character
min_lines = 2                         # minimum content lines in sticky mode

[scrollback.blocks.thinking]
animate = true                        # animated accent while thinking
truncated_lines = 3                   # lines in truncated mode
```

### Plugins

```toml
disable_plugins = false               # hide hooks/plugins UI entirely
```

---

## Environment variables

The key ones. See the README for the complete list.

### Authentication

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_API_KEY` | API key from console.echo-agent.invalid |
| `ECHO_AGENT_AUTH_PROVIDER_COMMAND` | External auth binary path |
| `ECHO_AGENT_AUTH_PROVIDER_LABEL` | Display name on TUI login screen |
| `ECHO_AGENT_AUTH_TOKEN_TTL` | Token lifetime in seconds |
| `ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS` | Seconds before expiry to refresh (default: 300) |
| `ECHO_AGENT_OIDC_ISSUER` | OIDC issuer URL |
| `ECHO_AGENT_OIDC_CLIENT_ID` | OIDC client ID |

### Endpoints

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL` | Override API proxy base URL |

### Features

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_MEMORY` | Enable (`1`) or disable (`0`) cross-session memory |
| `ECHO_AGENT_SUBAGENTS` | Enable (`1`) or disable (`0`) subagents |
| `ECHO_AGENT_WORKFLOWS` | Enable (`1`) or disable (`0`) background workflows and select the `/goal` driver (default on: host-owned workflow driver; off: legacy `update_goal`) |
| `ECHO_AGENT_WEB_FETCH` | Enable (`1`) or disable (`0`) the web_fetch tool |
| `ECHO_AGENT_WEB_FETCH_ALLOW_LOCAL` | Allow `web_fetch` to explicit loopback hosts only (`localhost` / `127.0.0.0/8` / `::1`). Same as `[toolset.web_fetch] allow_local`. Default off; private/metadata stay blocked. |
| `ECHO_AGENT_AGENT` | Custom agent definition path or name |
| `ECHO_AGENT_SANDBOX` | Sandbox profile (off, workspace, devbox, read-only, strict; or a custom profile name) |
| `ECHO_AGENT_EXIT_TIMEOUT_SECS` | Seconds after a quit is requested before the process is force-exited if teardown hangs (default: 20, `0` disables; a hard exit follows 5s later) |

### Logging

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_LOG_FILE` | Write logs to this file path (used verbatim as the path) |
| `RUST_LOG` | Log level filter (e.g. `debug`); controls the `ECHO_AGENT_LOG_FILE` log and headless stderr output |

### Paths

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_HOME` | Override config directory (default: `~/.echo-agent`) |
| `ECHO_AGENT_RESPECT_GITIGNORE` | Force gitignore filtering on (`1`) or off (`0`); overrides `[tools] respect_gitignore` |

### Telemetry

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_TELEMETRY_ENABLED` | Enable/disable telemetry |
| `ECHO_AGENT_TELEMETRY_TRACE_UPLOAD` | Enable/disable session trace upload |
| `ECHO_AGENT_TELEMETRY_MIXPANEL_ENABLED` | Enable/disable Mixpanel specifically |
| `ECHO_AGENT_EXTERNAL_OTEL` | External OTEL to your collector (see [24-monitoring-usage.md](24-monitoring-usage.md)) |
| `ECHO_AGENT_FEEDBACK_ENABLED` | Enable/disable feedback system |
| `ECHO_AGENT_DEPLOYMENT_KEY` | Management API key for enterprise |

---

## File locations

| Path | Description |
|------|-------------|
| `~/.echo-agent/config.toml` | Main configuration file |
| `~/.echo-agent/pager.toml` | TUI appearance configuration |
| `~/.echo-agent/auth.json` | Authentication credentials (auto-managed) |
| `~/.echo-agent/sessions/` | Persisted sessions (organized by working directory) |
| `~/.echo-agent/memory/` | Cross-session memory files and index |
| `~/.echo-agent/skills/` | User-scoped skill definitions |
| `~/.echo-agent/plugins/` | User-scoped plugins |
| `~/.echo-agent/agents/` | User-scoped agent definitions |
| `~/.echo-agent/lsp.json` | LSP server configuration (user-scoped) |
| `~/.echo-agent/logs/` | Internal log files (e.g. `unified.jsonl`, MCP server logs) |
| `.echo-agent/config.toml` | Project-scoped MCP servers, plugins, and permission rules |
| `.echo-agent/skills/` | Project-scoped skill definitions |
| `.echo-agent/plugins/` | Project-scoped plugins |
| `.echo-agent/agents/` | Project-scoped agent definitions |
| `.echo-agent/hooks/` | Project-scoped hooks |
| `.echo-agent/lsp.json` | LSP server configuration |

---

## Project-scoped configuration

Some settings can be set per-project by placing files in `.echo-agent/` inside your repository:

| File | What it configures |
|------|--------------------|
| `.echo-agent/config.toml` | MCP servers, plugins, permission rules, and the `[mcp] max_output_bytes` tool-result cap (other sections load only from `~/.echo-agent/config.toml`) |
| `.echo-agent/skills/` | Project-specific skills |
| `.echo-agent/hooks/` | Project-specific lifecycle hooks |
| `.echo-agent/agents/` | Project-specific agent definitions |
| `.echo-agent/lsp.json` | LSP server configuration |
| `.echo-agent/sandbox.toml` | Custom sandbox profiles |
| `AGENTS.md` | Project instructions (system prompt) |

Project-scoped MCP servers override global ones with the same name (full replacement, not a merge).

---

## LSP servers

Language servers power passive diagnostics and the optional `lsp` tool (see the [`lsp_tools`](#general-settings) feature flag). Definitions come from three sources and merge by server name:

| Source | Location | Scope |
|--------|----------|-------|
| User | `~/.echo-agent/lsp.json` | All projects |
| Project | `.echo-agent/lsp.json` | Current repository |
| Plugin | A trusted plugin's `.lsp.json` file, or an inline `lspServers` block in its `plugin.json` | Wherever the plugin is enabled |

When the same server name comes from more than one source, it resolves highest-priority first:

1. **Project** — `.echo-agent/lsp.json`
2. **User** — `~/.echo-agent/lsp.json`
3. **Plugins** — file-based `.lsp.json`, then inline `lspServers`, in plugin load order

Project and user entries replace lower-priority ones of the same name. Plugin entries only add servers whose names aren't already defined by a local file, so a local `lsp.json` always wins over a plugin. Plugin LSP servers load only after the plugin is trusted (see [Plugins](09-plugins.md)).
