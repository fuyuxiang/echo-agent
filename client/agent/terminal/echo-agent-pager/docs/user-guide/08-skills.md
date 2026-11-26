# Skills

Skills are reusable prompt packages that extend EchoAgent with task-specific instructions. They let you capture a repeatable procedure once, instead of re-explaining it each session.

---

## What Are Skills?

A skill is a directory that contains a `SKILL.md` file. Its markdown body tells EchoAgent how to handle a specific type of task: step-by-step instructions, conventions, and tool-usage patterns.

Use a skill for a repeatable procedure that's too specific for AGENTS.md but too long to retype. EchoAgent activates a skill only when it applies to your current task.

---

## Skill Locations

EchoAgent discovers skills from these directories, in priority order:

| Location | Scope | Priority | Notes |
|----------|-------|----------|-------|
| `./.echo-agent/skills/`, `./.echo-agent/commands/` | Local (CWD) | Highest | Current directory skills / legacy command markdown |
| `<repo_root>/.echo-agent/skills/`, `…/commands/` | Repo | Medium | Shared across the repo |
| `~/.echo-agent/skills/`, `~/.echo-agent/commands/` | User | Lowest | Personal skills for all projects |
| `~/.claude/skills/`, `~/.claude/commands/` | User | Lowest | Claude Code compatibility (configurable) |
| `./.claude/skills/`, `./.claude/commands/` | Local / Repo | High | Project Claude skills and legacy custom slash commands |
| `~/.cursor/skills/` | User | Lowest | Cursor compatibility (configurable) |
| `./.cursor/skills/` | Local / Repo | High | Project Cursor skills (when cursor compat skills are enabled) |

EchoAgent deduplicates skills by name -- a higher-priority location overrides a lower one. EchoAgent also scans `.agents/skills/` (and `commands/`) at each tier (alongside `.echo-agent/`) and walks every directory between your working directory and the repo root.

Flat `*.md` files under a `commands/` directory become user-invocable slash commands (filename stem = command name), matching Claude Code's legacy custom-command layout.

Skill and command discovery does **not** use `.gitignore`. Paths under known skill roots (`.echo-agent/`, `.agents/`, `.claude/`, `.cursor/`) always load when present on disk — teams often ignore `.claude/**` as local-only config while still expecting `/frontend`-style project commands to work. To hide a skill, use `[skills] ignore` in config (not repo ignore rules).

EchoAgent scans the Claude and Cursor skill directories by default. To stop scanning a vendor, set its `skills` cell to `false` under `[compat.cursor]` or `[compat.claude]` in `~/.echo-agent/config.toml`, or set the `ECHO_AGENT_CURSOR_SKILLS_ENABLED` or `ECHO_AGENT_CLAUDE_SKILLS_ENABLED` environment variable to `false`. See [Configuration](05-configuration.md#harness-compatibility) for details. EchoAgent always filters out known vendor-shipped default skills (such as Cursor's `shell`, `canvas`, and `statusline`), regardless of these settings.

### Additional Skill Directories

Add directories, exclude paths, or disable individual skills via `[skills]` in `~/.echo-agent/config.toml`:

```toml
[skills]
paths = ["~/my-team-skills"]          # Additional directories to scan
ignore = ["~/my-team-skills/wip"]     # Paths to exclude (hidden entirely)
disabled = ["wip-skill"]              # Skill names to keep listed but inactive
```

Each entry in `paths` is a `SKILL.md` file or a directory that EchoAgent walks recursively. `ignore` hides a skill completely; `disabled` keeps it in the list but excludes it from the system prompt and from invocation. `paths` and `ignore` take filesystem paths and support `~` expansion; `disabled` takes skill names.

---

## Creating a Skill

### Directory Structure

Each skill lives in its own directory with a `SKILL.md` file:

```
~/.echo-agent/skills/
  commit/
    SKILL.md
  review-pr/
    SKILL.md
  deploy/
    SKILL.md
```

### SKILL.md Format

A skill file has YAML frontmatter followed by markdown instructions:

```markdown
---
name: commit
description: Create well-formatted git commits following conventional commit standards. Use when the user wants to commit changes or asks for /commit.
---

# Git Commit Skill

Review staged changes and create a commit with a clear, conventional message.

## Steps

1. Run `git diff --staged` to see changes
2. Summarize what changed and why
3. Create commit message following conventional commits format
4. Run `git commit -m "..."` with the message
```

### Core Frontmatter Fields

| Field | Description |
|-------|-------------|
| `name` | Skill identifier. Use lowercase letters, digits, and hyphens, up to 64 characters. EchoAgent normalizes spaces and underscores to hyphens. If you omit `name`, EchoAgent uses the skill's directory name. |
| `description` | What the skill does and when to use it. EchoAgent reads this to decide whether to invoke the skill. If you omit it, EchoAgent uses the first paragraph of the body. |

Write a specific `description`. It determines when EchoAgent invokes the skill automatically. Name the trigger phrases and use cases.

### Optional Frontmatter Fields

Multi-word frontmatter keys use kebab-case (single-word keys like `model` are written as-is).

| Field | Description |
|-------|-------------|
| `when-to-use` | Trigger phrases for automatic invocation, kept separate from `description`. |
| `allowed-tools` | Tools the skill uses, as a YAML list or a comma- or space-separated string. |
| `argument-hint` | Hint text shown in the slash-command autocomplete (for example, `commit message`). |
| `user-invocable` | Whether you can run the skill as a slash command. Defaults to `true`; set `false` to hide it from slash commands. (To stop the model from invoking a skill, set `disable-model-invocation` instead.) |
| `disable-model-invocation` | When `true`, only your slash command runs the skill -- the model cannot invoke it automatically. Defaults to `false`. |
| `model` | Model override for running the skill. |
| `effort` | Reasoning-effort override. |
| `license` | License identifier (for example, `Apache-2.0`). |
| `compatibility` | Environment requirements (for example, `Requires git, docker, jq`). |
| `metadata` | Arbitrary string key-value pairs. EchoAgent promotes `metadata.author` and `metadata.short-description` for display. |

---

## Creating Skills with /create-skill

The `/create-skill` command walks you through building a new skill interactively. EchoAgent asks what you want, drafts the files, and writes them to disk.

### How It Works

When you run `/create-skill`, EchoAgent:

1. **Gathers requirements.** EchoAgent asks for the skill name, the scope to save it under, and a description of the workflow you want to capture. Use a name with lowercase letters, digits, and hyphens (2–64 characters, starting and ending with a letter or digit).

2. **Drafts the description.** EchoAgent writes a `description` that states what the skill does, the phrases that trigger it, and the slash command name. You approve or edit the draft before continuing.

3. **Creates the skill directory.** EchoAgent creates the `<scope>/.echo-agent/skills/<name>/` directory, plus `scripts/` or `references/` subdirectories when the skill needs them.

4. **Writes SKILL.md.** EchoAgent writes the frontmatter (`name` and `description`) and a markdown body of instructions, along with any supporting files.

5. **Verifies and confirms.** EchoAgent reads the file back, confirms it wrote correctly, and tells you how to run the skill.

### Choosing a Scope

EchoAgent asks where to save the skill:

- **Project** (`<repo_root>/.echo-agent/skills/<name>/`) -- available only in this repository and shareable with teammates through version control. EchoAgent recommends this scope inside a git repository.
- **User** (`~/.echo-agent/skills/<name>/`) -- available across all your projects.

To distribute a skill to a whole team or organization, package it in a plugin and publish it through a marketplace. See [Create your own marketplace](09-plugins.md#create-your-own-marketplace) and [Distribute across an organization](09-plugins.md#distribute-across-an-organization).

The new skill appears in the slash menu within a few seconds, because EchoAgent reloads skills when files change on disk.

---

## Using Skills

### Run a Skill by Name

Each skill is a slash command named after the skill. Run one by typing its name:

```
/commit              # Runs the "commit" skill
/review-pr           # Runs the "review-pr" skill
```

Running a skill loads its instructions into the conversation and directs the model to follow them. To pass arguments, type them after the name:

```
/commit fix the build
```

To browse your skills, type `/` to open the slash-command menu. EchoAgent lists every built-in command and skill and filters them as you type. To list skills from the command line instead, run `echoagent inspect` (see [Viewing Skill Details](#viewing-skill-details)).

### Qualified Names

When a skill's name collides with another skill or a built-in command, EchoAgent keeps **both** invocable. The built-in keeps the bare name (`/login`, `/compact`, …). The skill is advertised under a qualified name prefixed by its scope -- `local:`, `repo:`, `user:`, or the plugin name:

```
/local:commit        # The "commit" skill from ./.echo-agent/skills/
/user:commit         # The "commit" skill from ~/.echo-agent/skills/
/acme:login          # A plugin skill named "login" (built-in /login is unchanged)
```

Typing `/login` in the slash menu shows both rows, with a right-aligned **built-in** or **skill · plugin-name** badge so you can tell them apart. Rename the skill (or its directory) if you want the bare `/name` for the skill instead.

`echoagent inspect` tags colliding skills with `[collides with /login → /acme:login]`.

### Automatic Invocation

EchoAgent can invoke a skill on its own when it recognizes a relevant task. EchoAgent matches your prompt against the skill's `description` and `when-to-use` fields, so write both to describe the triggering situation.

For example, if a skill's description says "Use when the user wants to commit changes," then saying "commit my changes" can trigger that skill automatically. To require an explicit slash command and prevent automatic invocation, set `disable-model-invocation: true` in the frontmatter.

---

## Viewing Skill Details

Run `echoagent inspect` to see every skill EchoAgent discovers, along with the rest of your configuration:

```bash
echoagent inspect          # Human-readable summary
echoagent inspect --json   # Machine-readable report
```

In the human-readable output, the Skills section lists each skill's name and its source -- `project`, `user`, `bundled`, `config` (a `[skills].paths` entry), `server` (skills synced from the skill store in managed workspaces), or `plugin: <name>`. EchoAgent tags any skill disabled via `[skills].disabled` or from a disabled vendor surface with `[disabled]`.

The report honors your `[skills]` config the same way a live session does: skills from `paths` are listed, skills under an `ignore` prefix are hidden, and skills named in `disabled` stay listed but tagged `[disabled]`.

The `--json` report includes the full detail for each skill: its `name`, `description`, `source` (with the path to the SKILL.md file), and `userInvocable` flag. Skills whose bare slash name is contested — by a built-in command or by another skill — also include `collidesWith` (the contested name) and `invocableAs` (the qualified command to type).

---

## Bundled and Plugin Skills

EchoAgent distributes platform skills separately from your personal skills. Bundled skills are cached under `~/.echo-agent/bundled/skills/`; EchoAgent never writes them into `~/.echo-agent/skills/`. A same-named local, repo, or user skill overrides the bundled copy. `echoagent inspect` labels each definition by its actual source. (A plugin skill of the same name does not override a native skill; it stays available under its qualified `plugin:name` form.)

Skills can also come from plugins. When you install a plugin that includes skills, they appear alongside your user and project skills. `echoagent inspect` labels each plugin-provided skill with its source as `plugin: <name>`.

See the [Plugins guide](09-plugins.md) for more on installing plugins that provide skills.

---

## Best Practices

1. **Write specific descriptions.** The description drives automatic invocation. "Create git commits" is too vague; "Create well-formatted git commits following conventional commit standards. Use when the user wants to commit changes or asks for /commit." works better.

2. **Include concrete steps.** Skills work best when they give EchoAgent a clear, ordered procedure to follow.

3. **Reference tools by name.** When a skill relies on specific tools (such as `run_terminal_command` or `search_replace`), name them so the model knows what to use.

4. **Keep skills focused.** Write one skill per workflow. A "deploy" skill and a "rollback" skill work better than a single "deploy-and-rollback" skill.

5. **Version-control project skills.** Commit `.echo-agent/skills/` to your repository so the whole team benefits. User skills in `~/.echo-agent/skills/` stay personal and unshared.

6. **Test by running it.** Invoke `/name` and confirm the skill works before you rely on automatic invocation.
