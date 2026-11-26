# Make It Yours

## The easiest way: just ask

EchoAgent knows its own capabilities and can configure itself. Try:

- *"add the Postgres MCP server for our staging db"*
- *"switch to a light theme"*
- *"write an AGENTS.md for this repo"*

If you'd rather drive, everything below has a command too.

## Teach EchoAgent your project: AGENTS.md

Drop an `AGENTS.md` file in your repo root with build commands, conventions,
and gotchas. EchoAgent reads it automatically in every session — it's the single
highest-leverage customization:

```markdown
# My Project
- Run tests with `pnpm test`
- Never edit files under generated/
```

## Teach EchoAgent your facts: memory

Start a prompt with `#` (or use `/remember`) to save a note for future
sessions: `# the staging deploy uses eu-west`.

## Looks, keys, and extensions

- **`/theme`** — color themes (or `auto` to follow your OS); **`/settings`**
  (or `F2`) for everything else; **`/vim-mode`** if that's your thing.
- **Skills** (`/skills`) — reusable prompt packages; user-invocable skills
  become slash commands automatically.
- **MCP servers** (`/mcps`) and **plugins & hooks** (`/plugins`, `/hooks`).

Start with `AGENTS.md` and a theme; add the rest when you need it.

*Go deeper: `/docs Project Rules (AGENTS.md)`, `/docs Skills`, or `/docs MCP Servers`*
