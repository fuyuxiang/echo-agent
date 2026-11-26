# Agent Dashboard

The Agent Dashboard lists every top-level session in this pager process —
local sessions and forks — grouped by state. From one screen you can peek,
reply, attach, pin, rename, stop, or dispatch a new agent. Subagents are not
listed; they run under their parent, which already shows when work is in
flight.

Not the agents modal (`/config-agents` / `/agents` — definitions and
personas), the session picker (`/resume` / `F3`, past conversations on
disk), or the workflows run UI (`/workflow runs`).

---

## Opening the dashboard

- **`echoagent dashboard`** — launch the TUI into the dashboard.
- **`/dashboard`** (aliases **`/agents-dashboard`**, **`/sessions`**) — open
  from inside a session.
- **`Ctrl+\`** — same view as the slash command.

Hidden in minimal mode. Set `ECHO_AGENT_AGENT_DASHBOARD=0` or
`[dashboard].enabled = false` to disable.

---

## What you see

```
 EchoAgent Build · Dashboard — 4 agents · 2 awaiting
▌● reviewer · audit token flow    Awaiting your input            2m
 ● implementer · fix login bug    Running: cargo test           12m
 ⋅ refactor · feat/login          Responding…                   24m
 ○ housekeeping                   idle                           1h
 ● implementer · add login tests  8 tools · 1.2k tok            14m
╭─────────────────────────────────────────────────────────────────╮
│ ❯ Dispatch a new agent                                          │
╰─ dispatch ──────────────────────────────────────────────────────╯
 ↑/↓ select (peek) · Enter open · Ctrl+R rename · Ctrl+T pin · Ctrl+X stop · ? help · Esc new
```

Each row is a top-level agent. Sort by state (Needs input → Working → Idle →
Inactive → Completed → Failed) so same-state rows sit together, or by working
directory (`Ctrl+G` toggles). **Inactive** is roster-only sessions owned by
other pager processes that this process has not loaded — background noise, so
the section **starts collapsed** (expand with `→` / click).

To keep **Idle** scannable, only the most recent idle agents stay visible —
the 8 freshest, plus any active within the last hour. The rest fold into a
**"N more"** row at the bottom of the group; select it and press `Enter` /
`→` (or click) to expand, `←` to re-fold. The Idle header always shows the
true total. Folding is suspended while a filter or search is active.

State icons match other session lists in EchoAgent Build:

- `⋅`/`:`/`⸬`/`⁙` — animated spinner for **Working**
- `●` — filled circle for **Needs input**, **Completed**, **Failed**,
  **Blocked** (color: yellow / green / red / amber)
- `○` — hollow circle for **Idle** and **Inactive**

A row stays **Working** while it has live background work even if its turn
has finished — a background task, a `monitor`, or an active scheduled
`/loop`. The activity line says what is still running (for example
`1 monitor · 2 loops still running`).

There are no inline group headers; sort order keeps same-state rows adjacent,
and the per-row dot + color shows the group.

The dispatch input uses the same prompt chrome as the agent view. Press
`Ctrl+/` to flip it into **search mode**: the `❯` prefix becomes a yellow
`Search:` and typing live-filters the list instead of dispatching.

---

## Keybindings

| Key | Action |
| --- | --- |
| `↑` / `↓`, `j` / `k` | Navigate rows and section titles (selecting a row opens peek) |
| `→` / `←` (on a section title) | Expand / collapse the section (`l` / `h` in vim mode) |
| `Enter` (on a section title) | Toggle the section collapsed / expanded |
| `Enter` (empty reply) | Open the selected agent full-screen (details view) |
| `Ctrl+S` | Send the peek reply and open the agent (or dispatch and attach a new session) |
| `Shift+Enter` / `Alt+Enter` | Newline in the reply / dispatch input |
| `1`–`9` | Answer a pending permission / ask question when peek shows options |
| `Enter` (typed reply) | Send / queue the reply to the selected agent |
| `/` | Literal `/` into the prompt |
| `Ctrl+/` | Toggle search mode (live-filter rows) |
| `Ctrl+R` | Rename selected row |
| `Ctrl+T` | Pin / unpin |
| `Ctrl+G` | Toggle grouping (state ↔ directory) |
| `Ctrl+X` | Cancel a running turn, or press twice within 2s to permanently delete |
| Hover + click `[✗]` | Permanently delete an idle/done row (click again to confirm) |
| `Shift+↑` / `Shift+↓` | Reorder pinned rows |
| `Esc` | Step back: cancel search → close peek → clear filter → unfocus dispatch → unselect row → exit. Never clears a typed dispatch draft (`Ctrl+U` / `Ctrl+C` for that) |
| `Ctrl+\` | Return from details view, or exit dashboard |
| `Ctrl+.` (alt: `?`) | Keyboard shortcuts cheatsheet. Footer shows `?` when `Ctrl+.` cannot be delivered. Bare `?` opens help when list-focused or the draft is empty |

When grouping by state, each group has a **section title** (for example
`Working`, `Idle`) with a `▸`/`▾` marker. Select a title and press `→` /
`←` to expand or collapse (`l` / `h` in vim mode). Click toggles; hover
brightens. Collapse state is remembered while the dashboard stays open.
**Inactive** starts collapsed each time the pager starts; expanding it sticks
until you quit.

Opening a row shows the agent's conversation in the **details view**: a top
header (agent name; `{i}/{n}` cycle chips and `[Dashboard]` on the right)
above a full-width conversation — no bordered modal — so padding matches the
list view. Keys go to the attached agent; `Esc` / `Ctrl+\` (or `[Dashboard]`)
return to the dashboard; `[‹]` / `[›]` cycle agents. The shortcuts bar shows
`Ctrl+\: back to dashboard`. Gotcha: `Esc` only returns; `/exit` inside the
agent closes the session (dashboard toast: "Session closed").

`Ctrl+X` in the details view is state-dependent. While a **turn is running**
it cancels the turn (same as `Ctrl+C`, including the keep-subagents prompt)
and never closes the session. Otherwise — **idle**, a slash command in
flight, or a cancel still pending — `Ctrl+X` arms a confirmation: press again
within 2 seconds to close the session and return to the dashboard. Any other
key cancels the confirmation; a turn that starts inside the window turns the
confirmed press into a cancel instead. (If `Ctrl+X` is also the cheatsheet
binding on your terminal, use `Ctrl+.` inside the details view.)

See [Keyboard Shortcuts](03-keyboard-shortcuts.md#agent-dashboard).

---

## Completing or closing a session

There is **no** “mark completed” command. Row state is derived from the agent:

- **Completed** / **Failed** when work ends on its own (turn finished and no
  background task / monitor / `/loop` still running).
- **`Ctrl+X` once** while a turn is running cancels the turn.
- **`Ctrl+X` twice** (within 2s) **permanently deletes** the session
  (same as `/delete`). Hover an idle/done row to swap age for `[✗]` and
  click twice to confirm.
- In the details view, `/exit` also closes the session (Esc only returns).
  `/delete` inside an attached agent wipes that session and returns to the
  dashboard.

There is no manual complete flag. Use `/exit` to leave a session without
deleting history.

---

## Dispatch input

The bottom textarea **always spawns a new session**. A selected row is the
navigation cursor, not a reply target — open an agent to talk to it.

- Free text → new top-level session seeded with the prompt. Text is never
  treated as a filter (even if it starts with `/`, `s:`, `a:`, or `#`);
  filtering is `Ctrl+/` search mode. A leading `/` runs a pager-global slash
  command.
- Empty input → open the selected row, or create a new agent when
  `[+ New Agent]` is focused.

`Ctrl+S` after typing dispatches **and** attaches; plain `Enter` stays on the
dashboard so you can dispatch several sessions. `Shift+Enter` / `Alt+Enter`
insert a newline; the box grows with the draft (up to a cap, then scrolls).

Empty or whitespace-only prompts are ignored. Prompts above 64 KiB are
rejected with a toast.

### Focus: input bar ↔ overview list (`Tab`)

Two focus areas: the **dispatch input** and the **overview list**. `Tab`
toggles between them; the inactive input dims its border and hides its caret.

On open, focus defaults to the **overview list** when at least one agent
exists (so `↑`/`↓` / vim `j`/`k` navigate immediately). With **no** agents,
focus stays on the **dispatch input**. Either way, the cursor starts on
`[+ New Agent]` (no agent row pre-selected).

- **Input focused**: type a new-session prompt. Empty prompt: `↑`/`↓`
  navigate rows; non-empty: move the caret. `Esc` unfocuses to the list
  (draft kept).
- **Overview focused**: `↑`/`↓` (and vim `j`/`k`) move between rows. `Enter`
  opens the highlighted agent (on `[+ New Agent]`, sends a typed draft or
  creates a new session). `Esc` stays on the list and steps back — clear
  filter, then unselect (→ `[+ New Agent]`), then exit. `Tab`, `i` (vim), or
  any printable key returns to the input.

---

## Peek panel

Selecting an agent row shows the **peek panel** in place of the dispatch box.
With no row selected (`[+ New Agent]`, or after `Esc`), the dispatch box
returns. Select a row to talk to an existing agent; deselect to start a new
one.

Top to bottom: header (**last response type** — `Thinking` / `Thought` /
`Response` / `Edit` / `Read` / `Bash` / … — and **time**), the most recent
response (word-wrapped, up to ~3 rows; `…` when truncated), and a live
`❯ reply` input.

The selected agent's **model** and, in always-approve (yolo) mode, an
**`always-approve`** flag sit on the panel's bottom border (same badge slot as
the dispatch box), including while answering questions. List rows no longer
repeat model or always-approve badges.

**`Shift+Tab` cycles the peeked agent's mode** (Normal → Plan → Auto
(when enabled) → Always-approve → Normal) on the **live** agent. On the dispatch box,
Shift+Tab only stages mode for the *next* agent.

Unlike dispatch (new sessions only), peek reply **talks to the selected
agent**:

- **Type into `❯ reply`, then `Enter`** to send. Idle agents start immediately;
  busy agents **queue** the message (same as the agent view prompt). `Ctrl+S`
  replies and opens the detail view; `Shift+Enter` / `Alt+Enter` insert a
  newline (reply grows with the draft).
- Empty reply + `Enter` opens the agent.
- **`↑`/`↓` move the caret** once the reply has content. While empty (or
  unfocused via `Tab`), `↑`/`↓` **switch the selected agent** — the panel
  follows, and a half-typed draft is cleared so it cannot land on the wrong
  agent. (`Tab` to the list to navigate while a draft is in the reply.)
- **`Esc` unselects**: clear a typed reply first, then deselect and focus
  `[+ New Agent]`.
- **`Tab`** toggles focus between reply and row list; a printable key
  re-focuses the reply.
- Full prompt editor (same as dispatch / agent prompt): multi-line paste
  chips, mouse select, word navigation, `Ctrl+A`/`Ctrl+E`, `Alt+Backspace`,
  `Ctrl+W`/`Ctrl+U`/`Ctrl+K`, undo, Shift+arrow selection, `Ctrl+Shift+V`
  inline paste. **`@`** opens the file picker rooted at the **peeked agent's**
  working directory; the dropdown floats above the panel. Dashboard chords
  (`Ctrl+X` stop, `Ctrl+T` pin, `Shift+↑/↓` reorder, …) still win while the
  panel is open.
- Pending **permission / ask-tool** question: `❯ reply` hides; options list
  instead. **`↑`/`↓` highlight**, **`Enter` answers**, **`1`–`9`** answer
  directly. Free-text **No / reject** and ask-tool **Other** accept a typed
  answer on the free-text row. Multi-question Ask forms walk one at a time
  (`(i/N)`); multi-select forms need the agent's own view.

On very short terminals the panel may not fit; the dispatch box stays even
with a row selected.

---

## Search / filter (`Ctrl+/`)

`Ctrl+/` toggles search mode so normal typing always dispatches. Prefix
flips from `❯` to yellow `Search:`; every keystroke live-filters the list.

- `Enter` — confirm: keep the filter and return to the dispatch prompt.
- `Esc` or `Ctrl+/` — cancel: clear the filter and exit search.
- `↑` / `↓` — navigate filtered rows.

Prefixes (only inside search mode):

- `a:<name>` — agent label (case-insensitive substring; persona / role).
- `s:<state>` — row state: `working`, `idle`, `completed`, `failed`,
  `needs-input`, `blocked` and synonyms (`busy`/`running`/`done`/etc.).
- `#<text>` — substring match on `#<text>` (literal `#` in labels).
- anything else — substring over label + working dir.

---

## Persistence

Per-user preferences under `[dashboard]` in `~/.echo-agent/config.toml`:

```toml
[dashboard]
enabled = true
grouping = "state"   # or "directory"
pinned   = ["top:<session_id>", "sub:<parent_session_id>:<child_session_id>"]
reorder  = ["top:<session_id>"]
```

Pinned/reorder entries use **session id** (not a per-process agent slot), so
they survive restarts.
