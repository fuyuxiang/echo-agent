# Coming from Claude, Cursor, or Codex?

Fear not — your settings, rules, and skills come with you. EchoAgent Build
reads the same project conventions other agents use, and imports the rest.

## Picked up automatically

- **Rules & instructions** — `AGENTS.md` (the Codex/OpenCode convention),
  `CLAUDE.md` (including nested ones), and `*.md` rules under
  `.claude/rules/` and `.cursor/rules/`.
- **Skills & custom commands** — `~/.claude/skills/`, `~/.claude/commands/`,
  `~/.cursor/skills/`, and their project-level twins. Flat command `.md`
  files become slash commands here too.
- **MCP servers** — from `~/.claude.json`, `.cursor/mcp.json`, and project
  `.mcp.json`.
- **Hooks** — from `.claude/settings.json`, including matcher aliases like
  `Bash`, so most hooks run unchanged.

## One-step import

**`/import-claude`** scans your `~/.claude` settings — permissions, env
vars, MCP servers, hooks — and shows a checkbox preview; confirming
writes the items you selected into your `.echo-agent` config. Re-run it anytime.

## Pick up where you left off

The **`/resume-claude`**, **`/resume-codex`**, and **`/resume-cursor`**
skills continue a recent session from those tools right here.

## Check what was discovered

Run **`echoagent inspect`** in a repo to see every rules file, skill, and MCP
server EchoAgent picked up, tagged with where it came from. Each compat source
can be toggled in `[compat.claude]` / `[compat.cursor]` config sections.

And a few things you might have missed elsewhere: `/btw` asks a side
question without interrupting the current task, and `/rewind` rewinds the
conversation to an earlier turn (file changes stay as they are).

*Go deeper: `/docs Project Rules (AGENTS.md)`, `/docs Skills`, or `/docs MCP Servers`*
