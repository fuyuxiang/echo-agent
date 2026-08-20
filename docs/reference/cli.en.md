# CLI Commands

Complete reference for the `echo-agent` command-line interface.

## Synopsis

```bash
echo-agent <command> [subcommand] [options]
```

## Global Options

| Flag | Short | Description |
|------|-------|-------------|
| `--config <path>` | `-c` | Path to configuration file (default: `~/.echo-agent/config.yaml`) |
| `--verbose` | `-v` | Increase log verbosity (repeatable: `-vv`, `-vvv`) |
| `--quiet` | `-q` | Suppress non-error output |
| `--version` | | Print version and exit |
| `--help` | `-h` | Show help for any command |

---

## run

Start the agent in foreground mode. This is the primary entry point for running Echo Agent as a long-lived process.

```bash
echo-agent run [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--channel <name>` | Activate a specific channel only | All enabled |
| `--no-gateway` | Disable the HTTP gateway | Gateway enabled |
| `--port <port>` | Override gateway listen port | `3007` |
| `--profile <name>` | Load a named configuration profile | — |
| `--dry-run` | Validate config and exit without starting | — |

```bash
# Start with default config
echo-agent run

# Start with a specific config file, verbose logging
echo-agent run -c ./my-config.yaml -vv

# Start only the Telegram channel
echo-agent run --channel telegram

# Validate configuration without starting
echo-agent run --dry-run
```

!!! tip
    Use `echo-agent gateway install` for production deployments instead of running in the foreground. The gateway subcommand registers Echo Agent as a system service.

---

## setup

Interactive first-run wizard that guides you through initial configuration.

```bash
echo-agent setup [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--non-interactive` | Use defaults without prompting | — |
| `--channel <name>` | Pre-select a channel to configure | — |
| `--model <model>` | Set the default model provider | — |

```bash
# Launch interactive setup
echo-agent setup

# Non-interactive setup with defaults
echo-agent setup --non-interactive --model openai
```

!!! tip
    Re-run `setup` at any time to reconfigure. Existing settings are preserved as defaults in the prompts.

---

## status

Display the current agent status including uptime, active channels, memory usage, and session count.

```bash
echo-agent status [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--json` | Output as JSON | — |
| `--watch` | Refresh continuously | — |
| `--interval <sec>` | Watch refresh interval | `2` |

```bash
# Quick status check
echo-agent status

# Machine-readable output
echo-agent status --json

# Continuous monitoring
echo-agent status --watch --interval 5
```

---

## cost

Show cost analytics for model API usage across sessions.

```bash
echo-agent cost [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--period <range>` | Time range: `today`, `week`, `month`, `all` | `today` |
| `--by <dimension>` | Group by: `model`, `channel`, `session`, `tool` | `model` |
| `--json` | Output as JSON | — |
| `--limit <n>` | Max rows to display | `20` |

```bash
# Today's costs grouped by model
echo-agent cost

# This week's costs by channel
echo-agent cost --period week --by channel

# Export all-time costs as JSON
echo-agent cost --period all --json
```

---

## gateway

Manage the Echo Agent gateway as a system service. The gateway provides HTTP/WebSocket access and runs the agent as a background daemon.

```bash
echo-agent gateway <subcommand> [options]
```

### Subcommands

#### gateway (no subcommand) / gateway foreground

Run the gateway in the foreground (equivalent to `echo-agent run`).

```bash
echo-agent gateway
echo-agent gateway foreground
```

#### gateway install

Register Echo Agent as a system service (systemd on Linux, launchd on macOS, Windows Service on Windows).

```bash
echo-agent gateway install [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--user` | Install as a user-level service | System-level |
| `--name <name>` | Custom service name | `echo-agent` |
| `--config <path>` | Config file the service should use | `~/.echo-agent/config.yaml` |

```bash
# Install as user service
echo-agent gateway install --user

# Install with custom name
echo-agent gateway install --name echo-agent-prod
```

#### gateway uninstall

Remove the registered system service.

```bash
echo-agent gateway uninstall [--name <name>]
```

#### gateway start

Start the installed service.

```bash
echo-agent gateway start [--name <name>]
```

#### gateway stop

Stop the running service.

```bash
echo-agent gateway stop [--name <name>]
```

#### gateway restart

Restart the service (stop + start).

```bash
echo-agent gateway restart [--name <name>]
```

#### gateway status

Show service status (running, stopped, pid, uptime).

```bash
echo-agent gateway status [--name <name>] [--json]
```

#### gateway logs

Tail or display gateway logs.

```bash
echo-agent gateway logs [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--follow` / `-f` | Stream logs continuously | — |
| `--lines <n>` / `-n` | Number of lines to show | `50` |
| `--level <level>` | Filter by minimum log level | `INFO` |

```bash
# Tail logs
echo-agent gateway logs -f

# Last 100 warning+ lines
echo-agent gateway logs -n 100 --level WARNING
```

---

## cli

Launch an interactive CLI session connecting to a running agent (via gateway) or starting an embedded session.

```bash
echo-agent cli [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--host <host>` | Gateway host to connect to | `localhost` |
| `--port <port>` | Gateway port | `3007` |
| `--token <token>` | API token for authentication | — |
| `--session <id>` | Resume a specific session | New session |
| `--embedded` | Run without connecting to gateway | — |

```bash
# Connect to local gateway
echo-agent cli

# Connect to remote gateway
echo-agent cli --host myserver.local --port 3007 --token mytoken

# Resume previous session
echo-agent cli --session sess_abc123
```

---

## dashboard

Build and manage the web dashboard.

```bash
echo-agent dashboard <subcommand>
```

### Subcommands

#### dashboard build

Build the static dashboard assets.

```bash
echo-agent dashboard build [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--output <dir>` | Output directory | `~/.echo-agent/dashboard/` |
| `--minify` | Minify assets | Enabled |

```bash
echo-agent dashboard build
echo-agent dashboard build --output ./dist
```

!!! tip
    The dashboard is served automatically by the gateway at `/dashboard`. Use `dashboard build` only if you need to pre-build or customize the output location.

---

## cron

Manage scheduled cron jobs that the agent can execute autonomously.

```bash
echo-agent cron <subcommand> [options]
```

### Subcommands

#### cron list

List all registered cron jobs and their status.

```bash
echo-agent cron list [--json]
```

#### cron authorize

Authorize a pending cron job for execution.

```bash
echo-agent cron authorize <job-id>
```

#### cron revoke

Revoke authorization for a cron job, preventing future runs.

```bash
echo-agent cron revoke <job-id>
```

```bash
# List all cron jobs
echo-agent cron list

# Authorize a specific job
echo-agent cron authorize cron_daily_summary

# Revoke a job
echo-agent cron revoke cron_cleanup
```

!!! warning
    Cron jobs can invoke tools autonomously without user interaction. Always review job definitions before authorizing. Use `echo-agent cron list --json` to inspect the full tool chain a job will execute.

---

## eval

Run the evaluation suite against the agent to measure quality, latency, and tool-use accuracy.

```bash
echo-agent eval [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--suite <name>` | Run a specific eval suite | All suites |
| `--dataset <path>` | Path to evaluation dataset | Built-in dataset |
| `--output <path>` | Write results to file | stdout |
| `--format <fmt>` | Output format: `text`, `json`, `csv` | `text` |
| `--parallel <n>` | Concurrent eval workers | `4` |
| `--model <model>` | Override model for evaluation | Config default |

```bash
# Run all evaluations
echo-agent eval

# Run specific suite with JSON output
echo-agent eval --suite tool-accuracy --format json --output results.json

# Use custom dataset
echo-agent eval --dataset ./my-evals.yaml --parallel 8
```

---

## service

!!! danger "Deprecated"
    The `service` command is deprecated since v0.3.0. Use `gateway` instead. This command will be removed in v0.5.0.

```bash
echo-agent service <subcommand>
```

All subcommands are forwarded to their `gateway` equivalents. A deprecation warning is emitted on every invocation.

---

## plugin

Manage plugins that extend agent capabilities.

```bash
echo-agent plugin <subcommand> [options]
```

### Subcommands

#### plugin list

List all discovered plugins and their status.

```bash
echo-agent plugin list [--json] [--all]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--json` | Output as JSON | — |
| `--all` | Include disabled plugins | Only enabled |

#### plugin info

Show detailed information about a plugin.

```bash
echo-agent plugin info <plugin-name>
```

#### plugin enable

Enable a plugin.

```bash
echo-agent plugin enable <plugin-name>
```

#### plugin disable

Disable a plugin without uninstalling.

```bash
echo-agent plugin disable <plugin-name>
```

#### plugin check

Verify plugin dependencies and compatibility.

```bash
echo-agent plugin check [<plugin-name>]
```

```bash
# List enabled plugins
echo-agent plugin list

# Get plugin details
echo-agent plugin info web-search-enhanced

# Enable a plugin
echo-agent plugin enable web-search-enhanced

# Check all plugins for issues
echo-agent plugin check
```

---

## evolution

Manage the skill evolution system that automatically improves agent skills based on usage data.

```bash
echo-agent evolution <subcommand> [options]
```

### Subcommands

#### evolution status

Show current evolution state: active generation, fitness scores, pending candidates.

```bash
echo-agent evolution status [--json]
```

#### evolution run

Trigger an evolution cycle manually (normally runs on schedule).

```bash
echo-agent evolution run [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--skill <name>` | Evolve a specific skill only | All eligible |
| `--generations <n>` | Number of generations to run | `1` |
| `--dry-run` | Simulate without persisting | — |

#### evolution list-candidates

List candidate skill variants awaiting evaluation or promotion.

```bash
echo-agent evolution list-candidates [--json] [--skill <name>]
```

#### evolution show-candidate

Display the full definition and metrics of a candidate.

```bash
echo-agent evolution show-candidate <candidate-id>
```

#### evolution promote

Promote a candidate to replace the current skill version.

```bash
echo-agent evolution promote <candidate-id> [--force]
```

#### evolution rollback

Roll back a skill to its previous version.

```bash
echo-agent evolution rollback <skill-name> [--to-version <n>]
```

#### evolution init-dataset

Initialize the evaluation dataset for skill evolution from session history.

```bash
echo-agent evolution init-dataset [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--skill <name>` | Target skill | Required |
| `--sessions <n>` | Number of recent sessions to sample | `100` |
| `--output <path>` | Dataset output path | Auto |

```bash
# Check evolution status
echo-agent evolution status

# Run one evolution cycle
echo-agent evolution run

# List candidates, optionally filtered by status
echo-agent evolution list-candidates
echo-agent evolution list-candidates --status pending

# Inspect one candidate
echo-agent evolution show-candidate cand_7f3a2b

# Promote a winning candidate
echo-agent evolution promote cand_7f3a2b

# Roll a skill back to its pre-change version
echo-agent evolution rollback summarize

# Bootstrap evaluation data
echo-agent evolution init-dataset
```

The positional argument is a skill name for `rollback` and a candidate id for `show-candidate` and `promote`. Apart from `--status` (which filters `list-candidates`), this subcommand takes no flags of its own — there is no `--force`, `--generations` or `--to-version`.

Rollback restores the version retained at promotion time; the evolution engine keeps no per-version history to select from.

---

## skill

Manage skills that have been staged by the evolution system or installed from external sources.

```bash
echo-agent skill <subcommand> [options]
```

### Subcommands

#### skill list-staged

List skills awaiting human approval before activation.

```bash
echo-agent skill list-staged [--json]
```

#### skill approve

Approve a staged skill for activation.

```bash
echo-agent skill approve <skill-name> [--version <n>]
```

#### skill reject

Reject a staged skill, preventing activation.

```bash
echo-agent skill reject <skill-name> [--reason <text>]
```

```bash
# See what's pending
echo-agent skill list-staged

# Approve a skill
echo-agent skill approve daily-digest

# Reject with reason
echo-agent skill reject risky-tool --reason "Uses unrestricted shell access"
```

---

## config

Configuration inspection and management utilities.

```bash
echo-agent config <subcommand> [options]
```

### Subcommands

#### config dump

Dump the fully-resolved configuration (all layers merged).

```bash
echo-agent config dump [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--format <fmt>` | Output format: `yaml`, `json` | `yaml` |
| `--redact` | Mask secrets and tokens | Enabled |
| `--no-redact` | Show secrets in plaintext | — |

#### config explain

Show where each config value originates (which layer set it).

```bash
echo-agent config explain [<field-path>]
```

```bash
# Show origin of all fields
echo-agent config explain

# Show origin of a specific field
echo-agent config explain gateway.port
```

#### config validate

Validate the configuration file and report errors.

```bash
echo-agent config validate [--config <path>]
```

#### config gen-docs

Auto-generate configuration documentation from the schema.

```bash
echo-agent config gen-docs [--output <path>] [--format <fmt>]
```

```bash
# Dump resolved config as JSON
echo-agent config dump --format json

# Validate a specific file
echo-agent config validate --config ./staging.yaml

# Regenerate config reference docs
echo-agent config gen-docs --output docs/reference/configuration.en.md
```

---

## checkpoint

Manage agent state checkpoints for backup and recovery.

```bash
echo-agent checkpoint <subcommand> [options]
```

### Subcommands

#### checkpoint list

List available checkpoints.

```bash
echo-agent checkpoint list [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--json` | Output as JSON | — |
| `--limit <n>` | Max checkpoints to list | `20` |
| `--before <date>` | Filter checkpoints before date | — |

#### checkpoint show

Display details of a specific checkpoint.

```bash
echo-agent checkpoint show <checkpoint-id>
```

#### checkpoint restore

Restore agent state from a checkpoint.

```bash
echo-agent checkpoint restore <checkpoint-id> [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--dry-run` | Preview what would be restored | — |
| `--components <list>` | Restore specific components: `memory`, `skills`, `config`, `all` | `all` |

!!! danger
    Restoring a checkpoint overwrites current agent state. A pre-restore checkpoint is automatically created for rollback.

#### checkpoint prune

Remove old checkpoints to reclaim disk space.

```bash
echo-agent checkpoint prune [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--keep <n>` | Number of recent checkpoints to retain | `10` |
| `--before <date>` | Prune checkpoints older than date | — |
| `--dry-run` | Preview deletions without removing | — |

```bash
# List recent checkpoints
echo-agent checkpoint list

# Inspect a checkpoint
echo-agent checkpoint show chk_20250815_143022

# Restore memory only
echo-agent checkpoint restore chk_20250815_143022 --components memory

# Clean up old checkpoints
echo-agent checkpoint prune --keep 5
```

---

## migrate

Database and data migration utilities.

```bash
echo-agent migrate <subcommand> [options]
```

### Subcommands

#### migrate run

Apply pending migrations.

```bash
echo-agent migrate run [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--to <version>` | Migrate up to a specific version | Latest |
| `--dry-run` | Show SQL without executing | — |

#### migrate rollback

Roll back the most recent migration (or to a specific version).

```bash
echo-agent migrate rollback [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--to <version>` | Roll back to a specific version | Previous |
| `--steps <n>` | Number of migrations to roll back | `1` |

#### migrate status

Show migration status: applied, pending, and current version.

```bash
echo-agent migrate status [--json]
```

#### migrate memory-md

Export memory contents as Markdown files (useful for inspection or migration to other systems).

```bash
echo-agent migrate memory-md [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--output <dir>` | Output directory | `./memory-export/` |
| `--format <fmt>` | Format: `flat`, `nested` | `flat` |

```bash
# Apply all pending migrations
echo-agent migrate run

# Check migration status
echo-agent migrate status

# Dry-run to preview SQL
echo-agent migrate run --dry-run

# Roll back last 2 migrations
echo-agent migrate rollback --steps 2

# Export memory as markdown
echo-agent migrate memory-md --output ./backup/memory
```

---

## deps

Manage runtime dependencies (optional packages for specific features).

```bash
echo-agent deps <subcommand> [options]
```

### Subcommands

#### deps status

Show dependency status: installed, missing, version mismatches.

```bash
echo-agent deps status [--json]
```

#### deps install

Install missing optional dependencies for enabled features.

```bash
echo-agent deps install [options]
```

| Option | Description | Default |
|--------|-------------|---------|
| `--feature <name>` | Install deps for a specific feature only | All enabled |
| `--upgrade` | Upgrade existing packages to required versions | — |

#### deps refresh

Re-check and update the dependency lock state.

```bash
echo-agent deps refresh
```

```bash
# Check what's missing
echo-agent deps status

# Install all missing deps
echo-agent deps install

# Install only browser-related deps
echo-agent deps install --feature browser

# Upgrade outdated deps
echo-agent deps install --upgrade

# Refresh lock state after manual pip changes
echo-agent deps refresh
```

!!! tip
    Run `echo-agent deps status` after upgrading Echo Agent to identify newly required optional dependencies for features you use.
