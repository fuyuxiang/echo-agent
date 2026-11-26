# Slash Commands

Type `/` in the prompt to open the command menu. It fuzzy-matches as you type, and picking a command runs it immediately.

Commands come from two places: **shell builtins**, handled by the agent backend (echo-agent-runtime), and **pager builtins**, handled by the pager frontend (echo-agent-pager). Both show up in the same menu, and any enabled skill with `user-invocable: true` appears there too. If a skill reuses a built-in name such as `login`, the built-in keeps `/login` and the skill stays available as `/plugin-name:login` — the menu badges both so the collision is visible.

Every command below lists its aliases where it has them. A few commands only appear when a feature or session state enables them; those cases are called out inline. The menu is also filtered by render mode — see [`/minimal` and `/fullscreen`](#minimal-and-fullscreen).

---

## Session Management

### `/new`

Start a fresh session and clear the current conversation. Alias: `/clear`.

### `/resume`

Open the session picker to reload a previous session from disk.

### `/dashboard`

Open the [Agent Dashboard](23-dashboard.md): live roster of top-level sessions in this pager (peek, reply, dispatch, pin, rename, stop, attach). Aliases: `/agents-dashboard`, `/sessions`.

Not `/config-agents` (alias `/agents`), which manages agent *definitions* and personas. Hidden in minimal mode; disable with `ECHO_AGENT_AGENT_DASHBOARD=0` or `[dashboard].enabled = false`.

### `/compact [context]`

Compress conversation history to reclaim context-window space. Pass a note to tell EchoAgent what to keep:

```
/compact
/compact keep the auth implementation details
```

EchoAgent also auto-compacts once the context window hits 85% (tune it with `[session] auto_compact_threshold_percent`).

### `/context`

Show how the context window is being used: a category breakdown (system prompt, messages, reasoning and overhead, free space) plus informational rows for tool definitions, the skills listing, and MCP server announcements with their estimated token cost.

### `/session-info`

Show session details — auth method, model, turn count, and context usage. Aliases: `/status`, `/info`. Click a value or drag to select and copy; `c` copies the session ID and `y` copies the whole block.

### `/fork`

Branch the current session into a new agent, keeping history up to this point.

### `/rewind` (alias: `/undo`)

Roll the conversation back to an earlier turn and discard everything after it. `/undo` is the same command.

### `/copy`

Copy the most recent response's source markdown to the clipboard. Pass a number to copy the Nth-latest response instead, or a file path to write the text to a file rather than the clipboard (handy over SSH, where the local clipboard is often unreachable).

```
/copy
/copy 2
/copy out.txt
/copy 2 ~/exports/last-reply.md
```

Every copy is also written to a backup file — `~/.echo-agent/last-copy.txt` by default, or `ECHO_AGENT_COPY_FILE` if set. Confirmed copies toast briefly (e.g. `Copied!`). Unverified OSC 52 deliveries and clipboard-unreachable fallbacks name the backup path so you can recover the text.

### `/export`

Export the conversation to a file or the clipboard.

### `/quit`

Quit the application. Alias: `/exit`.

### `/home`

Leave the current session and return to the welcome screen. Alias: `/welcome`.

### `/delete`

Delete the current session's history. Confirms first. Stops any running turn, background tasks, and subagents before wiping history. Returns to the welcome screen, or to the dashboard when you opened the session from the dashboard.

To delete a session you are not in, open `/resume` or the welcome session list and press `d` then `y`. On the dashboard, press `Ctrl+X` twice or click `[✗]`.

### `/rename`

Rename the current session. Alias: `/title`.

```
/rename new session title
/rename --auto
```

`--auto` unpins a manual title and lets auto-titling resume. It applies to Build sessions only — chat conversations have no local auto-titler. It must be the only argument (`/rename --auto Something` is an error). A session cannot be named `--auto` via this command; use the dashboard rename editor (`Ctrl+R`) for that pathological case.

---

## Model and Mode

### `/model <name>`

Switch models. Accepts a model ID or display name (case-insensitive), and for reasoning models you can add an effort level as a second argument. Alias: `/m`.

```
/model echo-agent-build
/model EchoAgent Build
/model Reasoning X high
```

### `/effort <level>`

Set reasoning effort on the **current** model without reselecting it. Levels are `low`, `medium`, `high`, and `xhigh`, and it only applies when the active model supports reasoning effort.

```
/effort high
```

### `/always-approve` and `/auto`

Both are real toggles for the permission mode: they stay in the menu, and running the mode you're already in turns it back off.

| Command | When off | When already on |
|---|---|---|
| `/always-approve` | Skip all permission prompts | Back to ask |
| `/auto` | Classifier approves safe tools (dangerous ones may still prompt) | Back to ask |

Running one while the other is active switches modes — for example, `/auto` while always-approve is on switches to auto. `/auto` only appears when the auto permission-mode feature is enabled. You can also change mode with `Shift+Tab` (cycles Normal / Plan / Auto (when enabled) / Always-approve), `Ctrl+O`, or `/settings`.

### `/multiline`

Toggle multiline input. When it's on, `Enter` inserts a newline and `Shift+Enter` (or `Alt+Enter`) sends the message. Mid-turn, a bare `Enter` on an empty composer still force-sends the top queued follow-up. Alias: `/ml`.

### `/history`

Open prompt-history search: fuzzy-search this session's prompts newest-first, then press `Enter` or `Tab` to drop a match back into the prompt.

For quick recall, press `↑` on an empty prompt instead. With prompts queued, that moves focus into the queue pane, highlighting the last row; otherwise the panel opens with your most recent prompt already filled in, and `↑`/`↓` step through entries (each lands in the input), `↓` past the newest entry closes the panel, and typing edits the recalled prompt in place.

### `/compact-mode`

Toggle compact display — less padding and tighter spacing for denser output.

### `/vim-mode`

Toggle vim-style scrollback keys (`j`/`k`, `h`/`l`, `g`/`G`, `y`/`Y`, and so on). With it off (the default), a bare letter or `Shift+letter` in the scrollback just focuses the prompt and types the character. The setting persists to `[ui] vim_mode`.

### `/edit-prompt`

Open an external editor for the prompt, in either render mode. EchoAgent resolves `$VISUAL`, then `$EDITOR`, then `vi`; command values may include quoted arguments. Saving replaces the draft without sending it, and saving an empty file clears it. Typing `/edit-prompt` necessarily replaces the composer's contents, so the editor starts from an empty draft; to edit an **existing** draft, choose **Edit Prompt in External Editor** from the command palette (or press `Ctrl+G` in minimal mode), which preserves the text and refuses pasted, file-reference, or image chips without flattening them.

```
/edit-prompt
```

### `/minimal` and `/fullscreen`

Switch the current session to the other render mode, in place. `/minimal` (offered while you're in fullscreen) switches to the experimental scrollback-native mode; `/fullscreen` (offered while you're in minimal; alias `/full`) switches back to standard fullscreen mode. The switch happens inside the running process — nothing restarts, so a running turn keeps streaming and your composer draft, queued prompts, and permission mode all carry over; a marker (committed line in minimal, toast in fullscreen) reminds you how to switch back. Both are session-scoped — they don't touch `config.toml` — and the `--minimal` / `--fullscreen` CLI flags are session-scoped the same way. To make plain `echoagent` open in a given mode by default, use `/settings` → **Default screen mode** or set `[ui] screen_mode`. (If the in-place transition misbehaves in an exotic terminal, `ECHO_AGENT_SCREEN_MODE_SWITCH=exec` restores the old behavior of relaunching the pager onto the same session.)

A handful of commands only work in one of the two modes, because the surface they drive doesn't exist in the other: `/find`, `/jump`, `/timeline`, `/theme`, `/tutorial`, and `/dashboard` are fullscreen-only, while `/expand` is minimal-only. (`/workflow runs` is different: it opens the run pane in fullscreen and degrades to a text overview in minimal rather than refusing.) Those are hidden from the command menu and the palette in the mode they can't run in. If you type one out anyway, EchoAgent says why — and points you at whichever is actually useful. When the other mode is the only way to get it, that's the mode switch: `/theme isn't available in minimal mode (minimal renders with your terminal's own palette). Run /fullscreen to switch this session.` When this mode already does the job another way, it names that instead: `/expand isn't available in fullscreen mode: press Tab to focus the scrollback, then → on the block.` Everything else works in both. Note that `--no-alt-screen` still counts as fullscreen here, so it keeps the fullscreen-only commands.

### `/plan`

Enter plan mode.

```
/plan [description]
```

### `/view-plan`

Open a preview of the current saved plan. Aliases: `/show-plan`, `/plan-view`.

---

## Memory

`/flush`, `/dream`, and `/memory` require memory enabled through `ECHO_AGENT_MEMORY=1`, `[memory] enabled = true`, or managed remote settings; `/memory` also needs a configured memory backend. `/remember` is always available.

### `/memory`

Browse, view, and manage saved memories. Pass `on` or `off` to enable or disable memory. Alias: `/mem`.

```
/memory
/memory off
```

### `/flush`

Save the current session's knowledge to memory right now, triggering an LLM summary of the most important content. Reach for it before compaction, or any time you want to lock in context.

### `/dream`

Run memory consolidation — merge session logs into organized topics.

### `/remember`

Save a note to memory immediately, without waiting for an automatic summary.

```
/remember the staging deploy uses the eu-west cluster
```

---

## Hooks and Plugins

`/hooks`, `/plugins`, `/marketplace`, `/skills`, and `/workflows` all open the same extensions modal, each on its own tab.

### `/hooks`

Open the extensions modal on the Hooks tab, where you can view loaded hooks, add or remove custom ones, and toggle them individually. The modal does not grant project trust — see [10-hooks.md](10-hooks.md) for the trust model.

The shell also advertises individual `/hooks-list`, `/hooks-trust`, `/hooks-add`, `/hooks-remove`, and `/hooks-untrust` commands; in the pager these are folded into the `/hooks` modal.

### `/plugins`

Open the extensions modal on the Plugins tab to view installed plugins, install new ones from the marketplace, and manage trust.

The shell additionally supports subcommands (`/plugins list`, `/plugins install <source>`, `/plugins uninstall <name>`, `/plugins update`, `/plugins reload`). In the pager, the modal does the same work visually.

### `/marketplace`

Open the extensions modal on the Marketplace tab to browse and install plugins.

### `/skills`

Open the extensions modal on the Skills tab to view installed skills.

---

## Media Generation

### `/imagine <description>`

Generate an image from a text description.

```
/imagine a golden sunset over a calm ocean with silhouetted palm trees
```

### `/imagine-video <description>`

Generate a video from a text (or image) description. It plans shots, generates source images, and animates them with `image_to_video`.

```
/imagine-video a cat playing piano in a jazz club
```

---

## Scheduling

### `/loop [interval] <prompt>`

Run a prompt on a recurring interval. Give the interval as `30m`, `1 hour`, or `every 2 days`; leave it out and EchoAgent will ask.

```
/loop 30m check deploy status
/loop check deploy status every hour
```

Intervals are `Ns` (seconds, minimum 60), `Nm` (minutes), `Nh` (hours), or `Nd` (days); anything under 60 seconds is raised to the minimum. Recurring tasks expire after 7 days, and you can cancel one with `scheduler_delete` using the job ID reported when the loop is created.

---

## Workflows and Goals

### `/goal`

Set, manage, or check an autonomous goal. EchoAgent works across rounds and only marks the goal complete after an independent evidence review confirms the claim; if that review can't reproduce the result or has no usable evidence, the goal stays active or pauses with concrete gaps.

```
/goal Migrate the auth module to the new API
/goal status
/goal pause
/goal resume
/goal clear
```

Arguments are `<objective> [--budget <tokens>]`, or one of `status`, `pause`, `resume`, `clear`. The `--budget` here is a **token** budget for the goal run, separate from the agent-count budgets that workflows use. `/goal` appears when goal mode is enabled for the session. Which driver runs it depends on background workflows: with them on, the host evaluates each model round and runs adversarial verification on completion candidates; with them off, the legacy model-facing `update_goal` path reports progress and triggers verification.

### `/deep-research <query>`

Kick off a background research workflow. It plans a bounded set of questions, gathers structured claims with source evidence, cross-checks each claim on an independent verifier shard, and renders only the claims that survive, with their verified source locators. Failed shards, dropped claims, and researcher uncertainties are reported as coverage limitations, and the report is marked **Partial** whenever any remain.

```
/deep-research Compare the migration risks of PostgreSQL 17 and MySQL 9
```

The command returns right away — follow progress in `/workflow runs`, and the final report appears in the conversation on its own.

Workflows use an absolute cumulative `agent_budget` cap on logical child-agent calls: every `agent()` call and every item in a `parallel()` panel spends one slot, while schema-correction retries don't. The default is 128, explicit values run 1–1,024, and a panel that would cross the remaining budget is rejected before any of its children launch. Model-launched workflows set `agent_budget` on the `workflow` tool; named slash launches accept `--agent-budget N` or an `agent_budget` field in their JSON args. Named launches can also set child reasoning effort with `--effort LEVEL` or JSON `effort`, without changing the current session's `/effort`; a child script's own `effort` option takes precedence. Separately, a host-configured cap (32 by default) bounds how many children run at a time per run; larger panels queue and still act as a barrier. `budget()` reports the cap as `total`, admitted calls as `spent`, `reserved` (always zero), and `remaining`.

### `/workflow`

Launch a saved workflow, or manage a running one by its session-unique display name. Launch the same workflow twice and the display names are numbered (`review-changes`, `review-changes-2`); you never need the internal run IDs. Bare `/workflow` prints a text overview of this session's runs.

Type `/workflow` and a space to autocomplete saved workflow names (built-in, project, and user) plus the manage verbs `runs`, `pause`, `resume`, `stop`, and `save`. Picking a name fills it in and offers launch flags before you add args; it does not launch until you press Enter. `pause` / `resume` / `stop` / `save` then list this session's run handles — a bare `/workflow stop` does not pick a run.

```
/workflow review-changes --agent-budget 256 --effort high {"target":"origin/main...HEAD"}
/workflow review-changes {"target":"origin/main...HEAD","agent_budget":256,"effort":"high"}
/workflow runs
/workflow pause review-changes
/workflow resume review-changes
/workflow stop review-changes-2
/workflow save review-changes
```

`/workflow runs` opens the live **Workflow Runs** dashboard in the fullscreen TUI — active and retained runs, not a catalog of saved definitions. Each row shows the run's display name, phase, agent roster, progress, and result. Inside a run's detail view, `p` pauses, `r` resumes an ordinary pause, and `x` stops. Budget-limited runs can't bare-resume: `r` returns the shell's rejection (raise the cap with a model/tool resume that passes a higher `agent_budget`), while `x` still stops. `s` saves the run's script, but it's hidden for known built-ins and numbered duplicate handles — for those, choose a new unique `meta.name` and save the edited script explicitly. In minimal mode and non-TUI clients, `/workflow runs` prints the same text overview as bare `/workflow`.

Project workflows live in `.echo-agent/workflows/*.rhai`; user workflows live in `~/.echo-agent/workflows/*.rhai`. A same-process pause/resume continues the original immutable script, args, and `agent_budget` cap from committed host-call results — to iterate, edit the returned script copy and launch it as a new run.

A budget-limited run is different: it only resumes through a model/tool resume request that supplies an `agent_budget` above the admitted agent count. A bare `/workflow resume <name>` can't raise the cap, so it rejects budget-limited runs. Runs interrupted by a process restart aren't resumed at all, because external effects have no stable cross-process identity. And resume is not exactly-once: an external effect whose result wasn't committed before a same-process pause can run again.

### `/workflows`

Open the extensions modal on the **Workflows** tab — a browse-only catalog of the saved workflows EchoAgent discovered (built-ins, project `.echo-agent/workflows/`, and user `~/.echo-agent/workflows/`), with each entry's source, description, and path. The same catalog is listed for the model under the skill listing in the session preamble. Launch one with `/workflow <name>` (or its own slash command), then watch it in `/workflow runs`.

---

## Other

### `/theme`

Switch the color theme. Alias: `/t`.

### `/feedback [message]`

Report an issue or send feedback. Opens a report pane: `Enter` sends, `Esc` discards. A message prefills the pane so you can edit before sending. In `--minimal`, a message still sends immediately.

```
/feedback
/feedback Something isn't working correctly
```

### `/btw`

Send an aside to the agent without interrupting the current task. In minimal mode (`--minimal`), the answer shows up in a dismissible panel above the prompt: `Esc` dismisses it, a finished answer is saved into native scrollback, and a late reply to an already-dismissed panel is dropped. The side question and its answer aren't part of the main turn.

```
/btw also check the error handling
```

### `/mcps`

Open the MCP servers management modal.

### `/doctor`

Check the current session for terminal, clipboard, color, input, notification, and sandbox issues. Doctor shows what it found and how to resolve each issue. Run `/doctor fix` to list available automatic fixes; other findings include manual steps. `/terminal-setup`, `/terminal-check`, and `/terminal-info` remain aliases.

### `/release-notes`

View release notes for the current version. Alias: `/changelog`.

### `/docs`

Browse the built-in How-to Guides, open the online Build docs, or jump straight to a guide by title. Aliases: `/howto`, `/guides`.

```
/docs
/docs web
/docs Getting Started
```

- Bare `/docs` (or `/docs how-to`) opens the How-to Guides picker.
- `/docs web` opens https://docs.echo-agent.invalid/build/overview in your browser.
- `/docs <title>` opens a specific guide by case-insensitive title match.

### `/tutorial`

Open the onboarding tutorial: a short list of topics (your first prompt, attaching context, navigation, slash commands, worktrees, plan mode, customization, switching from another agent tool) — each a ~30-second read, with `→` flowing straight to the next topic. Nothing auto-shows — this command (or the command palette) is the way in.

```
/tutorial
```

Aliases: `/tour`, `/onboarding`

### `/import-claude`

Open the Claude import modal to bring over `~/.claude` settings: permissions, environment variables, MCP servers, hooks, and paths.

---

## Agents and Personas

### `/config-agents`

Open the agents modal to view and manage agent definitions, set the default, and switch the active one. Alias: `/agents`.

Not the live multi-session [Agent Dashboard](23-dashboard.md) (`/dashboard` / `Ctrl+\`).

### `/personas`

Create, edit, and delete personas. A subagent can apply a persona to shape how it behaves.

---

## Account and Billing

### `/login`

Log in or re-authenticate without leaving the session.

### `/logout`

Log out and return to the login screen.

### `/usage`

View credit usage or manage billing. Alias: `/cost`.

```
/usage
/usage manage
```

### `/privacy`

Open Settings on **Coding data, retention, and training**, where you choose
**Opt in** or **Opt out**. Takes no arguments.

```
/privacy
```

This setting doesn't touch `[features] telemetry`, `trace_upload`, or your external OTEL settings — see [Monitoring Usage](24-monitoring-usage.md#related-settings). On team accounts only a team admin can change it, and admins can also enable or disable Zero Data Retention for the team ([how to enable ZDR](https://docs.echo-agent.invalid/developers/faq/security#how-to-enable-zdr)). When the choice isn't yours to make, the row says so — `ZDR` or `· Admin Managed` — instead of opening the chooser.

---

## Configuration and UI

### `/settings`

Open the settings modal to view and change configuration interactively. Aliases: `/config`, `/preferences`, `/prefs`.

### `/timestamps`

Toggle message timestamps on or off.

---

## Skills as Slash Commands

Any enabled skill with `user-invocable: true` in its SKILL.md frontmatter shows up as a slash command. (Turn a skill off via `/skills` and it stops being advertised.) So a skill at `~/.echo-agent/skills/commit/SKILL.md` runs as:

```
/commit fix typo in README
```

Skills from plugins work the same way. When two skills share a name across scopes, qualify it:

```
/local:commit      # Project-scoped skill
/user:commit       # User-scoped skill
```

Built-in commands always win the bare name. Name a skill "compact" and `/compact` still runs the built-in — the skill stays available as `/local:compact` (or `/acme:compact` for a plugin). Both appear in the slash menu: the built-in is tagged `built-in` and the skill is tagged `skill · local` / `skill · acme`.

---

## Autocomplete

The menu supports fuzzy search: start typing after `/` to filter. Each entry shows the command name, its description, an argument hint when it takes arguments, and its source (builtin, skill scope, or plugin name). Press `Tab` or `Enter` to accept the highlighted command.
