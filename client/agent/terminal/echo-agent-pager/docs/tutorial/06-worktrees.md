# Parallel Work: Worktrees

Want EchoAgent working on a feature while you (or another EchoAgent session) work on
something else in the same repo? **Git worktrees** give each session its own
isolated checkout — no stepping on each other's changes, no stashing.

## Start a session in a worktree

- **From anywhere:** press `Ctrl+N` (twice to confirm) for a new session,
  then choose the worktree option.
- **From the welcome screen:** press `Ctrl+W` (inside a git repo) to open
  the New Worktree dialog.
- **From the shell:**

  ```bash
  echoagent --worktree=my-feature "refactor the auth module"
  ```

  (Use `=` — otherwise the prompt is taken as the worktree name.)

## Why this is great

- Run two or three EchoAgent sessions on the same repo simultaneously.
- Experiments stay isolated — if a change doesn't work out, your main
  checkout is untouched.
- When the work is done, apply the changes back like any git branch.

**`/fork`** copies your current conversation into a parallel session —
add a directive to point it at a task: `/fork try the async approach`.

Running several agents? The **dashboard** (`/dashboard` or `Ctrl+\`) shows
every session grouped by state — who needs input, who's working, who's done.

*Go deeper: `/docs Session Management`*
