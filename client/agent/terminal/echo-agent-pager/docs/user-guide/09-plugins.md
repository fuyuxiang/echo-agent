# Plugins

A plugin bundles skills, slash commands, agents, hooks, and MCP servers into one installable unit. You get plugins from a marketplace, install the ones you want, and EchoAgent loads what they add. To build and share your own, see [Create your own marketplace](#create-your-own-marketplace).

---

## How marketplaces work

A marketplace is a catalog of plugins that someone has published and shared. Using one takes two steps, like adding an app store: adding the marketplace lets you browse its plugins, and you then choose which to install.

1. **Add the marketplace** so EchoAgent can show what it offers. Nothing installs yet.
2. **Install the plugins you want**, one at a time.

Plugins stay off until you install and enable them, and a plugin's hooks and MCP servers stay inactive until you [trust](#trust-and-security) it.

---

## Add a marketplace

A marketplace source is a GitHub repository, a git URL on any host, or a local folder. Add one from the command line:

```bash
echoagent plugin marketplace add my-org/team-plugins                  # GitHub shorthand (owner/repo)
echoagent plugin marketplace add https://gitlab.com/acme/plugins.git  # any git host, include https:// and .git
echoagent plugin marketplace add ./my-marketplace                     # a local folder
```

List, refresh, and remove sources with `echoagent plugin marketplace list`, `echoagent plugin marketplace update [<name>]`, and `echoagent plugin marketplace remove <url>`.

You can also declare sources in config so they are always present.

### In config.toml

Each source needs a `name` and either a `git` URL (with an optional `branch`) or a local `path`:

```toml
[[marketplace.sources]]
name = "My Team Plugins"
git = "https://github.com/my-org/plugins.git"

[[marketplace.sources]]
name = "Local Dev"
path = "~/dev/my-plugins"
```

### In settings.json

Add sources under `extraKnownMarketplaces`, keyed by name. Each entry's `source` is one of `git` (with `url`), `github` (with `repo`), or `local` (with `path`):

```json
{
  "extraKnownMarketplaces": {
    "my-marketplace": {
      "source": { "source": "git", "url": "git@github.com:my-org/plugins.git" }
    }
  }
}
```

Place this file at `~/.echo-agent/settings.json` or `~/.claude/settings.json`.

---

## Install and use a plugin

Once a marketplace is added, install a plugin by name. You can also install straight from a repository or a local path:

```bash
echoagent plugin install deploy-tools --trust
```

The source you install accepts several forms:

- `owner/repo` (GitHub shorthand), `owner/repo@v1.0` (a ref), `owner/repo@<commit-sha>` (an exact commit, verified after fetch), or `owner/repo#subdir`
- a full git URL (`https://github.com/user/repo.git`) or SSH (`git@github.com:user/repo.git`)
- a local path (`./local-dir` or `/absolute/path`)

Run `echoagent plugin install <source>` without `--trust` and EchoAgent shows the source, warns that installing activates the plugin's hooks, MCP servers, and skills, then stops. Add `--trust` to go ahead. Only install plugins from sources you trust (see [Trust and security](#trust-and-security)).

A plugin's skills appear in the slash menu. When a skill name is ambiguous, EchoAgent shows the qualified form prefixed by the plugin name, for example `/deploy-tools:release`. To pick up a newly installed plugin, press `r` in the Plugins tab or start a new session.

---

## Manage plugins

### From the command line

```bash
echoagent plugin list [--json] [--available]   # installed plugins (--available requires --json)
echoagent plugin uninstall <name> [--confirm] [--keep-data]   # aliases: rm, remove
echoagent plugin update [<name>]               # omit the name to update every plugin
echoagent plugin enable <name>
echoagent plugin disable <name>
echoagent plugin details <name>                # show the plugin's component inventory
```

### In the terminal UI

Open the plugins modal with `Ctrl+L` (outside the VS Code family) or `/plugins` (any terminal, and required on the VS Code family). It has six tabs, **Hooks**, **Plugins**, **Marketplace**, **Skills**, **Workflows**, and **MCP Servers**; switch with `Tab` / `Shift+Tab`. The `/hooks`, `/marketplace`, `/skills`, `/workflows`, and `/mcps` commands open the modal on the matching tab.

In the **Plugins** tab, press `Enter` to expand a plugin and see its name, version, scope (`cli`, `project`, `user`, `custom path`, or the marketplace source name), skills, agents, hooks, MCP servers (shown as `blocked` when the plugin is not trusted), description, and path. Then:

| Key | Action |
|-----|--------|
| `r` | Reload all plugins |
| `a` | Add a plugin from `owner/repo`, a URL, or a local path |
| `Space` | Enable or disable the selected plugin |
| `x` | Uninstall the selected plugin |
| `f` | Filter by status (all, enabled, or disabled) |
| `/` | Search by name |

In the **Marketplace** tab, browse and install from your sources:

| Key | Action |
|-----|--------|
| `i` | Install the selected plugin |
| `d` | Uninstall the selected plugin |
| `a` | Add a marketplace source |
| `x` | Remove the selected source and its plugins |
| `r` | Refresh sources |
| `u` | Update the selected plugin |

Component summaries in the Marketplace tab appear only for marketplaces that publish a [`plugin-index.json`](#add-a-catalog-optional) catalog. Destructive actions ask for confirmation: press lowercase `y` to confirm, any other key (including `Esc`) to cancel.

In the **Workflows** tab (open it directly with `/workflows`, or `Tab` from the commands above), browse the saved workflows EchoAgent discovered: built-ins, project `.echo-agent/workflows/`, and user `~/.echo-agent/workflows/`. Each row shows the workflow's name, source, and description; press `Enter` to expand its path and when-to-use notes, `r` to reload the list, and `/` to search. Rows are browse-only — run one with `/workflow <name>` or its own slash command.

### Turn plugins on or off in config

Set these in `~/.echo-agent/config.toml`:

```toml
[plugins]
paths = ["~/my-plugins/custom-tools"]        # extra plugin directories
disabled = ["user/a1b2c3d4/noisy-plugin"]    # names or IDs to skip
enabled = ["project/9f8e7d6c/team-tools"]    # names or IDs to force on
```

Plugins are off by default, so list one in `enabled` to turn it on, or in `disabled` to discover it but skip loading it. Each entry is a plain plugin name (from `echoagent plugin list`) or a full ID (`<scope>/<hash>/<name>`).

To hide the plugins and hooks interface entirely, set `disable_plugins = true` in `~/.echo-agent/pager.toml`.

---

## Trust and security

Plugins run with your privileges, so treat them like any software you install: only add marketplaces and install plugins from sources you trust.

Enabling a plugin loads its skills, commands, and agents. Trust is separate and controls whether a plugin's code runs: even when enabled, its hooks, MCP servers, and LSP servers stay inactive until you trust it. EchoAgent trusts plugins in `~/.echo-agent/plugins/` automatically; project plugins in `.echo-agent/plugins/` require trust. Install with `--trust` to grant it:

```bash
echoagent plugin install <source> --trust
```

Trusted plugin `.mcp.json` servers attach to the session like other MCP config, and child agents inherit them. Plugin agents (`plugin-name:agent-name`) use the parent session's MCP servers by default, the same as user agents under `~/.echo-agent/agents/`; restrict that with the `mcpInheritance` frontmatter (see [Subagents](16-subagents.md#mcp-inheritance)). For safety, plugin agent frontmatter cannot declare `mcpServers` or hooks, or set `permissionMode: bypassPermissions`.

---

## Create your own marketplace

A marketplace is a git repository (or a local folder) that lists a set of plugins. Adding one works like adding an app store: it lets people browse your plugins, and they choose which to install. Publishing your own is how a team or an organization shares its skills, commands, agents, hooks, and MCP servers from one place.

You need three things: a git repository, one folder per plugin, and a single index file that lists them.

### Set up the repository

1. **Create a git repository.** A private repository is fine; access uses each person's own git credentials.
2. **Add each plugin as a folder.** A plugin folder holds any of `skills/`, `commands/`, `agents/`, `hooks/hooks.json`, `.mcp.json`, and an optional `plugin.json` manifest (see [What a plugin contains](#what-a-plugin-contains)).
3. **List the plugins in `.echo-agent-plugin/marketplace.json`.** This is the index EchoAgent reads.
4. **Push the repository.**

A typical layout:

```
my-org-plugins/
  .echo-agent-plugin/
    marketplace.json      # the index EchoAgent reads (required)
    plugin-index.json     # optional catalog for richer browsing
  plugins/
    gdrive/
      plugin.json         # optional manifest
      skills/gdrive/SKILL.md
      .mcp.json           # MCP servers this plugin adds
```

EchoAgent reads the index from `.echo-agent-plugin/marketplace.json`. It also accepts `.echo-agent-plugin/plugin.json` and the `.claude-plugin/` equivalents.

### Write the index

`marketplace.json` names the marketplace and lists each plugin:

```json
{
  "name": "My Org Plugins",
  "description": "Internal skills and tools",
  "owner": { "name": "Platform Team", "email": "platform@example.com" },
  "plugins": [
    {
      "name": "gdrive",
      "description": "Search and edit Google Drive, Docs, Sheets, and Slides",
      "category": "productivity",
      "source": { "type": "local", "path": "./plugins/gdrive" }
    }
  ]
}
```

Each plugin's `source` points at its files, in one of two ways:

- **In this repository**: `{ "type": "local", "path": "./plugins/gdrive" }`. The plain string `"./plugins/gdrive"` also works.
- **In a separate repository**: `{ "source": "url", "url": "https://github.com/my-org/gdrive.git", "sha": "<full commit sha>" }`. Pin a `sha` so installs are reproducible (required when you [require pinned versions](#require-pinned-versions)).

Optional per-plugin fields: `version`, `author`, `homepage`, `tags`, and `keywords`.

### Add a catalog (optional)

A `plugin-index.json` catalog lets the marketplace browser show each plugin's skills, commands, hooks, and agents before anyone installs it. It is for display only, installs work without it, and teams usually generate it in CI:

```json
{
  "version": 1,
  "plugins": {
    "gdrive": {
      "components": {
        "skills": [{ "name": "gdrive", "description": "Google Drive access" }]
      }
    }
  }
}
```

### Check and share it

Validate a plugin before publishing with `echoagent plugin validate [<path>]`, and tag a release from the manifest version with `echoagent plugin tag [<path>] [--push]`. Then point people at the repository. They add it once and install the plugins they want:

```bash
echoagent plugin marketplace add my-org/my-org-plugins   # GitHub shorthand, a git URL, or a local path
echoagent plugin install gdrive --trust
```

To install it for everyone automatically instead of person by person, see [Distribute across an organization](#distribute-across-an-organization).

---

## Distribute across an organization

Admins control plugins, marketplaces, and MCP servers through two managed layers the deployment sends to each user:

- **`managed_config.toml`** holds the same settings as a user's `config.toml` and merges into it. Use it to hand everyone a marketplace and turn plugins on.
- **`managed-settings.json`** is a protected policy file for allowlists and defaults. Its values take precedence over user, project, and local config and cannot be overridden.

### Roll a marketplace out to everyone

Add the source, and turn on the plugins you want, in `managed_config.toml`:

```toml
[[marketplace.sources]]
name = "My Org Plugins"
git = "https://github.com/my-org/my-org-plugins.git"

# Plugins stay off until enabled. List plugin names (from `echoagent plugin list`)
# or full IDs (`<scope>/<hash>/<name>`).
[plugins]
enabled = ["gdrive"]
```

For a hands-off install with no per-person step, also place the plugin's files where EchoAgent discovers and trusts them automatically: `~/.echo-agent/plugins/`, or a directory your device-management tool manages that you point to with `[plugins].paths`. Then enable them with `[plugins].enabled`.

A managed workspace can also sync skills to users directly, without a plugin. Synced skills appear with the `server` scope and are administered by the workspace; a user's own skill of the same name shadows the synced one. See [Skills](08-skills.md).

### Restrict which marketplaces can be added

List the only sources people may add in `managed-settings.json`. Any other marketplace is refused:

```json
{
  "strictKnownMarketplaces": [
    { "source": "git", "url": "git@github.enterprise.example:ACME/my-org-plugins.git" }
  ]
}
```

### Restrict which MCP servers can run

Also in `managed-settings.json`. Each entry allows an HTTP address (with `*` wildcards) or a local command; anything unlisted is denied. Wildcards match the host and the path separately — `https://*.example.com/*` cannot match a lookalike path on another host — and the scheme and port stay literal (an explicit `:443` on https and no port are the same target). Always include the scheme: a wildcard scheme (`*://…`) or a scheme-less pattern (`*.example.com/*`) never matches and logs a warning at startup. A pattern without a path matches only the root path; append `/*` to allow paths:

```json
{
  "allowedMcpServers": [
    { "serverUrl": "https://*.example.com/*" },
    { "command": "npx" }
  ]
}
```

The deployment can also send MCP servers to users directly. The allowlist bounds what any configuration, managed or personal, is allowed to run.

### Require pinned versions

Refuse any remote plugin install or update that is not pinned to a full commit sha:

```toml
[marketplace]
require_sha = true
```

You can also set `ECHO_AGENT_MARKETPLACE_REQUIRE_SHA=1`. Both only tighten the policy; neither turns it back off. Publish `sha` values in your marketplace's `plugin-index.json` so installs from it satisfy the rule. Plugins vendored directly inside a marketplace repository are copied from that repository's checkout, so pin them the same way, with `sha` values in `plugin-index.json`.

### Turn off the plugins UI

To hide the plugins and hooks interface, set this in `pager.toml`:

```toml
disable_plugins = true
```

### What this does not cover

Marketplaces distribute EchoAgent content: skills, commands, agents, hooks, and MCP server configurations. They do not install a program onto a machine. A skill or MCP server that runs a helper binary (for example a custom sign-in tool) still needs that binary delivered separately, bundled with your deployment or pushed through your device-management tool.

---

## Troubleshooting

**A plugin you installed isn't showing up.** Plugins are off until enabled. Check `echoagent plugin list`, then add the plugin's name or ID to `[plugins].enabled`, or press `Space` on it in the Plugins tab. Reload with `r` in the Plugins tab or start a new session.

**A plugin's hooks or MCP servers don't run.** They stay inactive until the plugin is trusted. Reinstall with `--trust`, or place the plugin under `~/.echo-agent/plugins/` (auto-trusted). See [Trust and security](#trust-and-security).

**A skill or MCP server from a marketplace is missing.** Refresh the source with `echoagent plugin marketplace update`, confirm the plugin is installed and enabled, and, if your organization restricts sources, check that the marketplace is still allowed (see [Distribute across an organization](#distribute-across-an-organization)). Some MCP servers require a sign-in and will not appear until you authenticate.

**An install is refused as unpinned.** Your deployment requires pinned commits. Install an exact commit (`owner/repo@<sha>`), or use a marketplace whose `plugin-index.json` publishes `sha` values. See [Require pinned versions](#require-pinned-versions).

**See exactly what loaded.** Run `echoagent inspect` (add `--json` for machine-readable output) to list every discovered plugin and the skills, agents, hooks, and MCP servers it provides, each labeled with its `plugin: <name>` source.

---

## Reference

### What a plugin contains

A plugin is a directory with any combination of:

- **Skills**: a `skills/` directory of SKILL.md files
- **Slash commands**: a `commands/` directory
- **Agents**: an `agents/` directory
- **Hooks**: a `hooks/hooks.json` file
- **MCP servers**: a `.mcp.json` file
- **LSP servers**: a `.lsp.json` file

An optional `plugin.json` manifest can override paths or add metadata; without one, EchoAgent discovers components from these standard directories. For example, a `team-tools` plugin might bundle a deploy skill, a code-review agent, pre-commit hooks, and a Linear MCP server, installed together in one step.

A skill or command may ship a **helper script** next to its SKILL.md (for example a Python file it calls). Put the script in the plugin and have the skill run it by relative path; it is copied to the machine with the plugin. The script's runtime and any packages it imports must already be present, plugins deliver files, not runtimes or native binaries (see [What this does not cover](#what-this-does-not-cover)).

### Where EchoAgent looks for plugins

EchoAgent discovers plugins from these locations, in priority order. The `.claude/plugins/` equivalents also work, and when two plugins share a name the higher-priority one wins:

| Location | Scope | Trust |
|----------|-------|-------|
| `_meta.pluginDirs` (`session/new` / `session/load`) | Session, that session only | Trusted automatically |
| `--plugin-dir` (the `echoagent agent … stdio` flag) | Process, that agent process only | Trusted automatically |
| `.echo-agent/plugins/` | Project, shared through version control | Requires trust |
| `~/.echo-agent/plugins/` | User, every project | Trusted automatically |
| `[plugins].paths` (config) | Custom directories you add | Depends on location |

The `_meta.pluginDirs` field on the `session/new` and `session/load` requests loads plugins for a single session; because the caller supplies the directory, those plugins are trusted automatically and do not persist after the session. `--plugin-dir` is the process-wide equivalent for a dedicated `echoagent agent … stdio` process, repeatable (`echoagent agent --no-leader --plugin-dir A --plugin-dir B stdio`), and ignored in leader mode, where the shared leader discovers its own plugins.

### Environment variables in plugin hooks

Plugin hooks receive two variables beyond the standard hook environment:

| Variable | Description |
|----------|-------------|
| `ECHO_AGENT_PLUGIN_ROOT` | Absolute path to the plugin's installed directory. |
| `ECHO_AGENT_PLUGIN_DATA` | Absolute path to the plugin's writable data directory, for state, caches, and logs. |

EchoAgent sets these and overrides any same-named value in the hook's `env` map (the `CLAUDE_PLUGIN_ROOT` and `CLAUDE_PLUGIN_DATA` aliases are set too). See the [Hooks guide](10-hooks.md) for every variable passed to hooks.

### Keyboard shortcuts

These keys work across every tab in the plugins modal:

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Next / previous tab |
| `j` / `k` or arrow keys | Move the selection |
| `Enter` | Expand or collapse the selected item |
| `/` | Search the current tab by name |
| `Esc` | Clear the search, or close the modal |
