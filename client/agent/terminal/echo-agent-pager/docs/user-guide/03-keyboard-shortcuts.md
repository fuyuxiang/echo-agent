# Keyboard Shortcuts

Reference for key bindings in the EchoAgent Build TUI. Bindings are built in and cannot currently be remapped.

---

## Input Modes

EchoAgent has two input modes that control how you navigate the scrollback:

- **Simple mode** (default): Arrow keys for navigation, `Shift+Arrow` for turn navigation, `Space` to focus the prompt, and any letter key auto-focuses the prompt.
- **Vim mode** (opt-in): `j`/`k` for navigation, `H`/`L` for turn navigation, `J`/`K` for response navigation, `h`/`l` for fold, `e`/`E` for expand/collapse, and `i`/`Tab`/`Space` to focus the prompt.

Simple mode is active by default. To switch to Vim mode, set `vim_mode = true` under `[ui]` in `~/.echo-agent/config.toml`, or toggle it at runtime with `/vim-mode`. See [Configuration](05-configuration.md) for details.

The tables below document bindings for both modes. The "Key" column shows the Vim-mode binding, and the "Alt Key" column shows the equivalent in simple mode (arrow keys, etc.).

> **Vim-mode required**: Single-letter and `Shift+letter` bindings in the
> **Scrollback** context (`j/k`, `h/l`, `g/G`, `L/H`, `y/Y`, `o/O`, `r`,
> `x`, `e/E`, and the `i` insert-mode alt) require `[ui].vim_mode = true`
> in `~/.echo-agent/config.toml` (or `/vim-mode` to toggle). Arrow keys, `Tab`,
> `Esc`, `Space`, `PageUp/Down`, and every `Ctrl+letter` shortcut work in
> both modes.

---

## Navigation (Scrollback Focused)

Move through conversation entries in the scrollback pane.

| Key | Alt Key | Action |
|-----|---------|--------|
| `j` | `Down` | Select next entry |
| `k` | `Up` | Select previous entry |
| `⇧L` | `Shift+Right` | Jump to next turn (user prompt) |
| `⇧H` | `Shift+Left` | Jump to previous turn (user prompt) |
| `⇧J` | | Jump to next assistant response |
| `⇧K` | | Jump to previous assistant response |
| `g` | | Go to top of scrollback |
| `⇧G` | | Go to bottom of scrollback |
| `Ctrl+K` | | Scroll up one line (without changing selection) |
| `Ctrl+J` | | Scroll down one line (without changing selection) |
| `PageUp` | | Scroll up one page (selection moves to the top of the viewport) |
| `PageDown` | | Scroll down one page (selection moves to the bottom of the viewport) |
| `Ctrl+U` | | Scroll up half page |
| `Ctrl+D` (`Shift+D` in VSCode) | | Scroll down half page |

`PageUp` and `PageDown` also scroll the conversation while the ordinary prompt
is focused, without moving focus or changing the draft. An active prompt
history, `@` file search, slash menu, or completion dropdown keeps the keys for
its own navigation.

---

## View (Scrollback Focused)

Control how entries are displayed in the scrollback.

| Key | Alt Key | Action |
|-----|---------|--------|
| `h` | `Left` | Collapse selected entry |
| `l` | `Right` | Expand selected entry |
| `e` | | Toggle fold on selected entry |
| `⇧E` | | Expand all / collapse all entries |
| `Ctrl+E` | | Expand/collapse all thinking blocks |
| `r` | | Toggle raw markdown on selected entry |

Setting `respect_manual_folds = true` under `[scrollback.scroll]` in
`pager.toml` (opt-in, off by default — see
[Configuration](05-configuration.md)) makes a hand-folded block pinned:
streaming updates and finish events (for example a thinking block ending)
leave it alone instead of resetting it, and expanding a block while
auto-scroll is following the tail stops following so you can read; resume
with `⇧G`, `j` at the last entry, scrolling past the bottom, or sending a new
prompt. `⇧E` clears all pins, and `Ctrl+E` clears pins on thinking blocks.

### Block Content

| Key | Action |
|-----|--------|
| `y` | Copy block content to clipboard |
| `⇧Y` | Copy block metadata (e.g., the shell command) to clipboard |
| `Enter` | Open block content in fullscreen viewer |
| `Ctrl+F` | Open block content in fullscreen viewer (alt binding) |

---

## Focus

Switch between the prompt input and scrollback pane.

| Key | Alt Key | Context | Action |
|-----|---------|---------|--------|
| `Tab` | `Space` (and `i` in vim mode) | Scrollback focused | Focus the prompt input |
| `Tab` | | Prompt focused | Focus the scrollback (both simple and vim scrollback modes) |
| `Tab` | `Shift+Tab` (backwards) | A blocking card is focused (question, permission prompt, cancel-turn panel) | Walk that card's rows, wrapping round at the ends. Focus stays in the card |
| `Tab` | `Space` (and `i` in vim mode) | Scrollback focused with a card parked | Hand the keyboard back to the card (the bar's focus hint names it) |
| `Enter` | | Prompt focused | Send the current prompt |

**Esc is not a focus key.** It follows the cancel / clear / rewind semantics below. The mid-turn cancel is the only branch gated on `[ui].vim_mode` (scrollback nav); nothing depends on `[ui].simple_mode` (prompt editor). Overlays, modals, slash/file dropdowns, voice, search, and selection still steal Esc first.

## Blocking cards

Four surfaces block the agent on your answer and take over the keyboard while
they are open: the **question card** (`ask_user_question`), the **MCP
elicitation card** (`echo.agent/mcp/elicit`), the **permission prompt**, and the
**cancel-turn panel**. When more than one is open the permission prompt has the
keyboard first, then the cancel-turn panel, then question, then elicitation —
and the shortcuts bar always shows the keys of whichever one is receiving them.

They share one contract:

- `Tab` / `Shift+Tab` walk that card's rows and wrap at both ends. They never
  move focus out of the card, so the cursor is always somewhere you can see.
- `Esc` steps back out, one rung at a time: it clears whatever the card has
  pending first, and only once there is nothing left to clear does it leave.
  Where it leaves to is the one thing that differs per card — the question card
  and the permission prompt park the keyboard in the scrollback so you can
  scroll up and read the context behind them (the card stays on screen), while
  the cancel-turn panel's "keep running" closes the panel and leaves the
  turn (and any subagents) running. Enter or `1`–`4` still pick a
  cancel-and-subagent choice.
- With the keyboard parked, the shortcuts bar shows the scrollback's own keys,
  and its focus hint names the card rather than the prompt: `Tab/Space:
  question`. That hint is pinned, so a narrow bar can never trim away the only
  route back.
- Inside the dashboard's session overlay there is one more rung: once the
  keyboard is parked, the next `Esc` returns to the dashboard, leaving the card
  pending. (`Ctrl+\` still leaves from any state.)

### MCP elicitation card (`echo.agent/mcp/elicit`)

Shown when an MCP server asks for user input (form fields or URL consent).
The title always includes the MCP server name.

| Key | Action |
|-----|--------|
| `↑` / `↓`, `j` / `k`, `Tab` | Move between fields / actions |
| `Space` / `Enter` on a field | Edit text, or toggle boolean / enum |
| `←` / `→` on actions | Accept vs Decline |
| `Enter` on Accept | Submit (form validates first; URL opens the browser) |
| `d` / Decline | Decline the request |
| `Esc` | Step back: leave text editing first, then park focus in the scrollback (`Tab` returns). Only while waiting on an accepted URL does it dismiss the card |
| `Ctrl+C` | Cancel the request |
| `o` | Reopen URL while waiting for completion |

### Question card (`ask_user_question`)

| Key | Action |
|-----|--------|
| `↑` / `↓`, `j` / `k` | Move between answers (clamped at the ends) |
| `Tab` / `Shift+Tab` | Walk this question's answers in a loop — off the last answer back to the first. It never carries you into another question |
| `←` / `→`, `h` / `l`, `[` / `]` | Previous / next question |
| `1`–`9`, `a`–`f` | Pick that answer directly |
| `z` | Jump to the free-text row and start typing |
| `Space` | Toggle the focused answer (multi-select), or start typing on the free-text row |
| `Enter` | Select and advance, submit on the last question, or edit the free-text row |
| `Esc` | Unselect this question's answer; with nothing selected, park focus in the scrollback (`Tab` returns). On the *first* question inside the dashboard's session overlay it returns to the dashboard instead — from a later question `←` is still the way back, so the park comes first and the next `Esc` leaves. The shortcuts bar names whichever rung is live |
| `y` | Copy the focused answer |
| `Shift+X` | Dismiss the question (the agent continues without an answer) |
| `Ctrl+F` | Fullscreen the card |

The `/feedback` pane is the one exception to this table: the report box has no
answers to walk, `Enter` sends the report, and `Esc` dismisses the pane. When a
trace upload can be offered, `Enter` on the report first shows an upload
question (`↑`/`↓` choose, `Enter` sends with your choice, `Esc` skips the
upload and still sends the report).

While typing a free-text answer, `Enter` submits and `Esc` returns to the
answer rows; every other key goes to the text field.

### Permission prompt

| Key | Action |
|-----|--------|
| `↑` / `↓`, `j` / `k` | Move between options (clamped at the ends) |
| `Tab` / `Shift+Tab` | Walk the options in a loop |
| `1`–`9` | Choose that option directly |
| `Enter` | Choose the focused option |
| `←` / `→` | Widen / narrow the scope an "always" answer would remember |
| `e` | Edit the always-allow pattern by hand (bash prompts) |
| `Ctrl+F` | Expand / collapse the full arguments |
| `Ctrl+O` | Turn on always-approve mode |
| `Esc` | Park focus in the scrollback (`Tab` returns). It never answers or dismisses the request |
| `Ctrl+C` | Cancel the request |

Typing on the "No" row starts a message back to the agent instead; `Enter`
sends it and `Esc` returns to the options.

### Cancel-turn panel

| Key | Action |
|-----|--------|
| `↑` / `↓`, `j` / `k`, `Tab` / `Shift+Tab` | Move between the choices |
| `1`–`4`, `Enter` | Confirm that choice |
| `Esc` | Keep everything running. This resolves the panel, so it is never a dead end and never needs to park |

## Escape

| State | Gesture | Effect |
|--------|---------|--------|
| Turn running, **minimal mode or vim scrollback mode off (the default)** | `Esc` | Cancel immediately (prompt or scrollback focused, even with a draft — the draft is **preserved**, unlike Ctrl+C's clear-first gesture). |
| Turn running, **fullscreen vim mode** | `Esc` | Swallowed no-op (does **not** cancel). Use `Ctrl+C` (or palette / other cancel entry points). |
| Turn cancelling | `Esc` | Re-sends cancel in **every** mode (retry if the first ack was lost). `Ctrl+C` in this state escalates toward quit. |
| Idle + non-empty prompt (text or image chips), **prompt focused** | **2× `Esc` within 800ms** | Clear the prompt; the cleared draft is stashed (`Ctrl+S` or `Alt+S` restores it, images included) and its text ranks first in the `↑` history browse. First press shows “press again to clear”. |
| Idle + empty prompt + conversation messages, **prompt or scrollback focused** | **2× `Esc` within 800ms** | Open the rewind picker (same as `/rewind`). First press is silent (no toast). |
| Idle + empty + no messages, **or scrollback focused with a draft / moded (`!` `#`) composer / pending needs-input overlay / open history search** | `Esc` | Swallowed no-op (does not focus scrollback). Clear is prompt-pane only; rewind requires an empty Normal-mode composer, no pending overlay, and no open history search. Reading the scrollback never mutates your draft, your composer mode, a question awaiting an answer, or an in-progress search. |

**Post-cancel grace:** for about a second after an Esc-triggered cancel, the idle rewind arm stays suppressed — mashing Esc to stop a turn cannot silently open the rewind picker. Only the rewind arm is held; every other Esc behavior is unaffected.

**Steal-Esc (runs before mid-turn cancel / swallow and clear / rewind):** overlays, modals, slash/file/completion dropdowns, history search, scrollback search, text selection, link highlight, voice, and **Bash / Remember mode exit** when the prompt is empty (Esc leaves `!` / `#` mode and returns to the normal prompt, even while a turn is running). Bare `/feedback` opens the report pane; Esc dismisses it.

**Ctrl+C vs Esc:** with a non-empty draft while a turn is running, Ctrl+C clears the draft and keeps the turn; a second Ctrl+C on an empty prompt cancels. Esc cancels immediately and preserves the draft (in fullscreen vim mode it does not cancel; it only retries while already cancelling). Idle non-empty Ctrl+C clears in one press; Esc requires two presses within 800ms. The two clears differ in what they leave behind: `Esc Esc` stashes the draft, so `Ctrl+S` brings it back, while `Ctrl+C` discards it (its text is still in the `↑` history).

---

## Agent-Level

Actions that affect the agent session, available from the agent screen.

| Key | Context | Action |
|-----|---------|--------|
| `Ctrl+P` | Agent screen | Open the command palette |
| `?` (Shift+/) | Agent screen | Open the command palette (alt binding) |
| `Ctrl+M` | Agent screen | Open the model picker / switch model |
| `Ctrl+M` | Prompt focused | Toggle multiline input mode |
| `Ctrl+C` | Agent screen | Cancel the current turn (or clear non-empty draft first; see Escape table) |
| `Ctrl+O` | Agent screen | Toggle always-approve (YOLO) mode |
| `F3` | Agent screen | Open the session picker (resume a previous session, same as `/resume`) |
| `Ctrl+;` (alt: `Ctrl+'`) | Agent screen | Toggle the prompt queue pane (when non-empty). **Local macOS** VS Code family only: primary **`Ctrl+4`** (`;` / `'` still alts). SSH and non-Mac keep **`Ctrl+;`** / **`Ctrl+'`**. |
| `Shift+Tab` | Prompt focused | Cycle mode (Normal → Plan → Auto (when enabled) → Always-approve) |
| `Ctrl+B` | Agent screen | Send the running foreground command to the background |
| `Ctrl+T` | Agent screen | Toggle the todos pane |
| `Ctrl+G` | Agent screen (full TUI) | Toggle the tasks pane |
| `Ctrl+G` | Ordinary composer (minimal mode) | Edit the current draft in an external editor without sending it. If the terminal reserves this chord, choose **Edit Prompt in External Editor** from the command palette. |
| `Ctrl+L` | Agent screen | Open the extensions modal (**non–VS Code family only**; on VS Code / Cursor / Windsurf / Zed, `Ctrl+L` is mid-turn **interject** and extensions open via `/plugins` / `/hooks`) |
| `↑` | Prompt focused (empty prompt, normal input mode) | With prompts queued, move focus into the queue pane with the last row highlighted (`e` edits it, `Enter` sends it now). Otherwise open the history panel with your last prompt filled in; `↑`/`↓` step through entries (each lands in the input), `↓` at the newest closes the panel, and typing edits the recalled prompt in place. Recalled `!` shell commands re-enter shell mode. `↓` never opens history. |
| `Ctrl+S` (alt: `Alt+S`) | Prompt focused | Stash / pop the draft, `git stash`-style. With text or images in the composer: stash it and start fresh. On an empty composer: restore the newest stash (images and `!` shell mode included). A chord-stashed draft also **restores automatically after you send your next prompt** (a double-Esc-cleared draft stays stashed, since that gesture is a discard). One draft at a time: a new stash replaces the old one, whose text stays reachable in the `↑` history; the stashed draft's text ranks first there. |
| `!` | Prompt focused | Enter shell mode (type `!` on an empty prompt) |
| `Ctrl+.` (alt: `Ctrl+X`) | Agent screen | Open the keyboard shortcuts help |
| `F2` (alt: `Ctrl+,` / `Cmd+,`) | Agent screen | Open the settings modal |

**Note:** `Ctrl+M` is context-dependent. When the prompt is focused, it toggles multiline input mode. Otherwise, it opens the model picker.

**Note:** While a draft is stashed, the prompt's top border reads `Stashed` (next to the `/rename` title, if you set one). Minimal mode draws no border, so it prints a line in the scrollback each time you stash or restore. The stash lives in memory only: it is gone when you quit, and it does not travel to a resumed session. A new stash replaces the old one, and only the old one's **text** moves to the `↑` history, so any images on the replaced draft are lost.

**Note:** External editing works in every render mode: minimal mode binds `Ctrl+G`, and the full TUI uses `/edit-prompt` or the command palette. EchoAgent resolves `$VISUAL`, then `$EDITOR`, then `vi`. Values may include quoted arguments. Saving replaces only the draft (the final newline editors append on save is stripped); an empty file clears it. Drafts with pasted/file/image chips must be edited in the composer so attachments are not flattened.

**Note:** `Ctrl+'` is a Windows alt for `Ctrl+;` — some Windows consoles drop the `Ctrl` modifier on punctuation keys.

**Note:** `Ctrl+.` needs the Kitty keyboard protocol (or tmux `extended-keys on` so that protocol can pass through). On VS Code / Cursor / Windsurf / Zed integrated terminals, VTE, Apple Terminal, Windows Terminal, JetBrains, tmux with `extended-keys off`, screen, and similar no-KKP setups, EchoAgent advertises **`Ctrl+X`** as the primary shortcuts-cheatsheet key instead. **`Ctrl+X` always works** as a classic control character even when `Ctrl+.` does not. Run `/doctor` if modified keys misbehave in tmux.

---

## Image Paste & Drag-and-Drop

| Action | macOS | Linux | Windows |
|---|---|---|---|
| Drag image from file manager into the prompt | Finder ✓ | Files / Dolphin ✓ | Explorer ✓ |
| Copy a file in the file manager, then paste | `Cmd+V` | `Ctrl+V` | `Ctrl+V` |
| Screenshot or "Copy Image" in clipboard, then paste | `Cmd+V` | `Ctrl+V` | **`Alt+V`** |

Non-image files insert their absolute path as text instead of a chip.

> **`Alt+V` on Windows** is echo-agent-specific. Windows Terminal's default `Ctrl+V` only pastes plain text and silently drops image clipboards; `Alt+V` bypasses the interceptor. To use `Ctrl+V` for images too, add `{ "command": null, "keys": "ctrl+v" }` to `actions` in your Windows Terminal `settings.json`.

### Linux PRIMARY and CLIPBOARD

Linux X11 has two independent text selections:

- `Ctrl+V` reads **CLIPBOARD**, the explicit copy/cut selection. It never falls back to PRIMARY. To put text there with `xclip`, use `printf %s "text" | xclip -selection clipboard`.
- An unmodified middle click in EchoAgent reads **PRIMARY**, the current mouse selection, only when `DISPLAY` is non-empty. Pure X11 can use its native reader fallback; XWayland requires `xclip` or `xsel` on `PATH` so EchoAgent reads the X11 selection rather than Wayland PRIMARY. The press is handled once; the release does not paste again.
- `Shift+Insert` is the terminal-native way to paste selected text. Many terminals also use `Shift+middle click` to bypass application mouse reporting.

Over SSH, the remote EchoAgent process usually cannot access the terminal's local X11 selection. Use terminal-native `Shift+Insert` or `Shift+middle click` so the local terminal sends the selected text through the PTY.

---

## During an active turn (agent running)

While the agent is generating:

- **Plain `Enter`** (with text in the composer) **queues** a follow-up for later. By default (`[ui].follow_up_behavior = "queue"`) those follow-ups run after the current turn ends — and they deliberately **hold** while the agent is blocked waiting on background tasks or a subagent (a hint explains the hold and how to send one now). With `"steer"`, the same Enter still shows the row in the queue, then the shell injects it mid-turn at the next tool or model safe gap (see [Configuration](05-configuration.md)).
- **`Enter` again on the emptied composer** (double-Enter) sends the **top** queued follow-up now.
- The **send now** chord is **cancel-and-send**: it stops the current turn (background tasks, subagents, and the rest of the queue keep running) and sends your message as the next turn, so it always appears at the bottom of the transcript:
  - **Non-empty composer** → cancel and send that text now.
  - **Empty composer** + a queued follow-up → send the **top** queued follow-up now (no need to focus the queue pane). On the queue pane, the same chord (or the **[Send now]** button) sends the **selected** row.
  - **Idle**, or **empty composer with nothing queued** → no-op for that key.
- While the agent is **blocked waiting** (on task output or a subagent), plain `Enter` with text also delivers immediately — the shell cancels the blocked turn and runs your message next.

| Terminal | Primary | Alternates | Action |
|----------|---------|------------|--------|
| Default | `Ctrl+Enter` | `Ctrl+I` | Send now (cancels the current turn, runs your message next) |
| Apple Terminal | `Ctrl+O` | `Ctrl+Enter`, `Ctrl+I` | Send now |
| VS Code family (VS Code, Cursor, Windsurf, Zed) | **`Ctrl+L`** | *(none)* | Send now (`Ctrl+I` not used — Tab / host chat; plugins via `/plugins`) |

In `/multiline` mode, `Shift+Enter` (or `Alt+Enter`) sends while plain `Enter` inserts a newline — except on an **empty** composer mid-turn with a queued follow-up, where plain `Enter` still **send now**s the top row (same as normal mode). (`Ctrl+Enter` is send-now mid-turn when bound on non–VS Code family; it does not submit a new idle turn.)

Send-now is intentionally interruptive — it reads as "stop what you're doing and take this". To hand the agent a note **without** stopping it, queue with plain `Enter`; the agent picks it up at the next turn boundary.

> **WezTerm**: These modified Enter keys need `enable_kitty_keyboard = true` in your WezTerm config. Full steps and a one-line workaround are in the [terminal support guide](21-terminal-support.md#problem-ctrlenter-doesnt-interject-in-wezterm).

> **Windows (non–VS Code family)**: Some consoles drop the `Ctrl` modifier on `Ctrl+Enter` (it can collapse to bare `Enter` or `Ctrl+J`). Use `Ctrl+I` as the alt — letter-key Ctrl chords are stable everywhere. On VS Code family, use **`Ctrl+L`**.

> **VS Code family `Ctrl+L`**: EchoAgent uses it for interject and leaves the extensions shortcut unbound (open plugins with `/plugins` or the command palette). If your terminal profile still maps **Clear** (or another command) to `Ctrl+L`, that host binding can steal the chord — rebind or remove it so the PTY receives form feed (`\x0c`).

---

## Global

Actions available from any screen.

| Key | Alt Key | Action | Confirmation |
|-----|---------|--------|-------------|
| `Ctrl+N` | | Create a new session (optionally in a git worktree) | Yes (double-press within 1000ms) |
| `Ctrl+\` | | Open or toggle the [Agent Dashboard](23-dashboard.md) | No |
| `Ctrl+Q` | `Ctrl+D` | Quit the application | Yes (double-press within 1000ms) |

**VS Code family terminal** (VS Code, Cursor, Windsurf, Zed integrated terminals): `Ctrl+Q` is captured by the host, so EchoAgent makes **`Ctrl+D` the sole quit key** (`Ctrl+Q` is not bound). Half-page-down is rebound to bare **`Shift+D`**. Mid-turn interject uses **`Ctrl+L`** (no alternates) because `Ctrl+Enter` / `Ctrl+I` do not reliably reach the PTY; extensions are opened via `/plugins` instead of `Ctrl+L`.

> **Returning to the welcome screen has no key binding** — use the `/home` slash command (alias `/welcome`) from inside a session. See [Slash Commands](04-slash-commands.md).

### Destructive Action Confirmation

Actions marked with "Yes" in the confirmation column require a double-press within 1000ms. Press the key once to see a confirmation prompt, then press again to confirm. This prevents accidental session loss.

---

## Welcome Screen

Bindings that only fire on the welcome screen (before any agent session is open).

| Key | Action |
|-----|--------|
| `F3` | Resume session (open the session picker) |
| `Ctrl+W` | Open the New Worktree dialog (only inside a git repository) |
| `Ctrl+I` | Import Claude settings (when available) |
| `Ctrl+Shift+I` | Dismiss the Claude import row (when available) |

`Ctrl+W`, `Ctrl+I`, and `Ctrl+Shift+I` are only active on the welcome screen. `F3` opens the session picker on both the welcome screen and inside an agent session (where it opens as a modal overlay, same as the `/resume` command). `Ctrl+S` is the prompt stash inside a session (it does nothing on the welcome screen, which has no draft to set aside). `Ctrl+Q` is the same global Quit binding documented above, not a welcome-specific handler.

---

## Agent Dashboard

Bindings while the [Agent Dashboard](23-dashboard.md) is focused (`Ctrl+\` or `/dashboard`).

| Key | Action |
|-----|--------|
| `↑` / `↓`, `j` / `k` | Navigate agent rows (selecting a row opens peek) |
| `Enter` | Open the selected agent, or send a typed peek reply / dispatch prompt |
| `Ctrl+S` | Reply or dispatch **and** attach to that agent |
| `Ctrl+/` | Toggle search / filter mode |
| `Ctrl+R` | Rename the selected agent |
| `Ctrl+T` | Pin / unpin |
| `Ctrl+G` | Toggle grouping (state ↔ working directory) |
| `Ctrl+X` | Cancel a running turn, or press twice within 2s to permanently delete |
| `Ctrl+O` | Toggle always-approve on the selected agent |
| `Tab` | Toggle focus between the list and the dispatch / peek input |
| `Esc` | Step back (cancel search → close peek → clear filter → unfocus → unselect → exit) |
| `Ctrl+\` | Exit the dashboard (or return from an attached agent) |
| `Ctrl+.` (alt: `?`) | Shortcuts cheatsheet |

Details (peek vs dispatch, search prefixes, persistence): [Agent Dashboard](23-dashboard.md).

---

## Command Palette

Press `Ctrl+P` or `?` to open the command palette -- a searchable list of common actions, each showing its key binding or slash command. It includes session actions, the extensions modal tabs (Hooks, Plugins, Marketplace, Skills, Workflows, MCP Servers), and more.

Type to filter, then press `Enter` to execute the selected action.

---

## Shortcuts Bar

The bottom of the TUI displays a contextual shortcuts bar showing the most relevant key bindings for the current state. The hints change based on:

- Which pane is focused (scrollback vs. prompt)
- Whether the agent is currently running
- What type of entry is selected

---

## Mouse Support

The TUI supports mouse interaction:

- **Click** on a scrollback entry to select it
- **Scroll wheel** to scroll through the scrollback
- **Click** on the prompt area to focus it
- **Hover** over the prompt to see a highlight (configurable via `pager.toml`)
- **Middle click** on Linux X11/XWayland to paste the PRIMARY selection

---

## Quick Reference Card

### When scrollback is focused (Simple mode — default)

```
Navigation:       Up/Down (prev/next entry)  Shift+Left/Right (prev/next turn)
Scrolling:        Ctrl+J/K (line)  PgUp/PgDn (page)  Ctrl+U/D (half page)
Focus prompt:     Space or any letter key (auto-focuses and types)
```

### When scrollback is focused (Vim mode)

```
Navigation:       j/k (up/down)  H/L (prev/next turn)  K/J (prev/next response)  g/G (top/bottom)
Scrolling:        Ctrl+J/K (line)  Ctrl+U/D (half page; D=Shift+D in VSCode)  PgUp/PgDn (page)
Folding:          h/l (collapse/expand)  e (toggle)  E (all)
Content:          y (copy)  Y (copy cmd)  Enter (fullscreen)
View:             r (raw markdown)  Ctrl+E (thinking)
Focus prompt:     i, Tab, or Space
```

### When prompt is focused

```
Send:             Enter
Newline:          Shift+Enter or Alt+Enter
Multiline:        Ctrl+M (toggle)
Paste:            Ctrl+V (text, files, screenshots on macOS/Linux)
Selected text:    Middle click or Shift+Insert (Linux X11/XWayland PRIMARY)
Paste image:      Alt+V (Windows only — for screenshots / "Copy Image")
Select all:       Cmd+A (macOS, Ghostty only — see note below)
Select text:      Shift+←/→ (char) · Alt+Shift+←/→ (word) ·
                  Cmd+Shift+←/→ (visual row) · Shift+Home/End (logical line) ·
                  Shift+↑/↓ (row)
Copy / Cut:       Cmd+C / Cmd+X (with a selection; Kitty-protocol terminals)
Leave:            Tab (back to scrollback)
Cancel (running): Ctrl+C (empty prompt; non-empty draft clears first)
Clear (idle):     Esc Esc within 800ms (non-empty prompt)
Rewind (idle):    Esc Esc within 800ms (empty prompt + messages)
```

With a selection active, typing / `Enter` / paste replace it, delete and
word-kill chords delete just the selection, arrows collapse it to the
matching edge (word/line moves continue from that edge), and `Esc` or `Tab`
drop the highlight while still performing their normal action. Note
`Shift+←/→` only selects while the **prompt** is focused; with scrollback
focused the same chords jump between turns (see Navigation above).

> **Cmd+A is gated to Ghostty.** EchoAgent's in-app `Cmd+A` handler is only
> wired up when the detected terminal is Ghostty. Other terminals
> either swallow `Cmd+A` at the terminal layer (Apple Terminal, default
> iTerm2) or apply their own in-terminal "Select All" behaviour (Kitty,
> WezTerm). On a non-Ghostty terminal, the binding does nothing and the
> key falls through to the terminal's native behaviour.
>
> On Ghostty, add the one-line unbind to `~/.config/ghostty/config` so
> the keystroke reaches the running TUI:
>
> ```ini
> keybind = cmd+a=unbind
> ```
>
> After Ghostty reloads (it watches the config file), `Cmd+A` in the
> prompt selects every character in the prompt buffer, including pasted
> image chips. Image chips are always path-free (`[Image #N]`); the
> filepath (when known) appears only in the image preview overlay on
> hover or when the cursor is on/right after the chip.

### Always available

```
Command palette:  Ctrl+P or ?
Model picker:     Ctrl+M (from scrollback)
Cancel:           Ctrl+C (see Escape table)
Always-approve:   Ctrl+O (toggle YOLO)
New session:      Ctrl+N (press again, then choose normal/worktree)
Quit:             Ctrl+Q (or Ctrl+D in VSCode)
```
