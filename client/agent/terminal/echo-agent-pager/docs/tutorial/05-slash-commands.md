# Slash Commands

Type `/` on an empty prompt and a searchable dropdown of commands appears.
A few worth knowing on day one:

| Command | What it does |
|---------|--------------|
| `/help` | Browse every command and keyboard shortcut |
| `/model` | Switch models or reasoning effort |
| `/resume` | Pick up a previous session where you left off |
| `/new` | Start a fresh session |
| `/compact` | Compress a long conversation to free up context |
| `/btw` | Send EchoAgent an aside *without* interrupting its current task |
| `/rewind` (alias `/undo`) | Rewind the conversation to an earlier turn |
| `/docs` | Full How-to Guides, in the TUI or on the web |
| `/feedback` | Send feedback to the team |

Two of those deserve a second look:

- **`/compact`** takes an optional hint: `/compact keep the auth details`.
  Check context usage anytime with `/context` — EchoAgent also auto-compacts
  when the window fills up.
- **`/rewind`** (or **`/undo`**) rewinds the conversation to an earlier
  turn, dropping later turns (file changes are left as-is).

## The command palette

Press **`Ctrl+P`** (or `?` from the scrollback) to open the command palette —
one searchable list of every command, shortcut, and skill. There's also a
full shortcuts cheatsheet on `Ctrl+.` (use `Ctrl+X` if your terminal
swallows it).

You don't need to memorize anything: `/` and `Ctrl+P` will always show you
what's available.

*Go deeper: `/docs Slash Commands`*
