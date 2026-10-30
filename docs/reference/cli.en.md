# CLI Command Reference

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

This page follows the current `echo-agent` argument parser. Use `echo-agent --help` and `echo-agent <command> --help` for the installed version.

## Global options

```bash
echo-agent [--version] [-c CONFIG] [-w WORKSPACE] <command> ...
```

`-c/--config` and `-w/--workspace` can precede the main command or follow subcommands that support them. There is no global `--verbose`, `--quiet`, or `--log-level` option.

## Commands

| Command | Purpose |
|---|---|
| `run` | Start the full agent in the foreground |
| `setup` | Run configuration flows or a specific setup section |
| `status` | Show configuration and runtime capability summary |
| `cost` | Show cost attribution and trends |
| `gateway` | Run the Gateway or manage its background service |
| `cli` | Connect an interactive terminal to the local Gateway |
| `dashboard build` | Build the Web Dashboard |
| `cron` | List, authorize, or revoke scheduled jobs |
| `eval` | Run an evaluation dataset |
| `plugin` | Inspect or toggle plugins |
| `evolution` | Manage skill evolution |
| `skill` | Review staged skills |
| `config` | Inspect, explain, validate, or document configuration |
| `checkpoint` | Inspect and restore workspace file checkpoints |
| `migrate` | Migrate USER memory ownership or import legacy memory shards |
| `deps` | Manage skill dependencies |
| `service` | Deprecated compatibility alias for Gateway service commands |

## Agent and Gateway

```bash
echo-agent run
echo-agent run --force
echo-agent status --json
echo-agent cost --days 7 --json
echo-agent gateway --host 127.0.0.1 --port 58123
echo-agent gateway install
echo-agent gateway start
echo-agent gateway status
echo-agent gateway logs --follow
echo-agent gateway stop
echo-agent gateway uninstall
```

`run --force` bypasses the single-instance workspace guard. `cost` accepts `--days` (default 7) and `--json`; it has no `--since`, `--until`, or `--group-by` options. Omitting the Gateway action runs it in the foreground. `--host` and `--port` apply to the foreground command. Gateway service actions are `install`, `uninstall`, `start`, `stop`, `restart`, `status`, and `logs`. `--system` selects a Linux system-level service; `--force` can regenerate its service file.

## Setup and terminal client

```bash
echo-agent setup
echo-agent setup gateway --lang zh
echo-agent setup doctor --json
echo-agent setup --flow quickstart
echo-agent cli --inline
echo-agent cli --tui
echo-agent cli --port 58123 --token YOUR_TOKEN --user alice
echo-agent dashboard build --force
```

`setup` accepts `--lang en|zh|auto` and `--flow quickstart|full`. `--json` applies to the `doctor` section. The terminal client connects to loopback; remote access requires an SSH port forward. `--inline` is the default; `--tui` needs the `tui` extra. `dashboard build` accepts `--force`, but no `--output` or `--dev` option.

## Scheduled jobs and evaluation

```bash
echo-agent cron list
echo-agent cron authorize JOB_ID -y
echo-agent cron revoke JOB_ID -y
echo-agent eval -d dataset.jsonl -t smoke -p 3 -o results.json
```

`cron authorize` and `cron revoke` edit persisted scheduler state and should be used while the resident service is stopped. During service operation, use Dashboard or conversation commands. The CLI has no `cron add` action. `eval` takes the dataset through `-d/--dataset`, not a positional argument.

## Plugins, evolution, and skills

```bash
echo-agent plugin list --json
echo-agent plugin check
echo-agent plugin info NAME
echo-agent plugin enable NAME
echo-agent plugin disable NAME
echo-agent evolution status
echo-agent evolution list-candidates
echo-agent evolution show-candidate CANDIDATE_ID
echo-agent evolution promote CANDIDATE_ID
echo-agent evolution rollback SKILL_NAME
echo-agent skill list-staged
echo-agent skill approve CANDIDATE_ID
echo-agent skill reject CANDIDATE_ID --reason "not ready"
```

`evolution` also has `run` and `init-dataset`. `skill list-staged` lists candidates awaiting review; the CLI does not have `skill list`. Installed skills are visible in the Dashboard.

## Configuration and checkpoints

```bash
echo-agent config dump --format yaml
echo-agent config explain gateway.port
echo-agent config validate
echo-agent config gen-docs
echo-agent checkpoint list --json
echo-agent checkpoint show SHA
echo-agent checkpoint restore SHA -y
echo-agent checkpoint prune
```

`config dump` defaults to YAML and also supports JSON. `checkpoint` actions are `list`, `show`, `restore`, and `prune`. Restore changes workspace files and asks for confirmation unless `-y/--yes` is supplied. `prune` has no `--keep` or `--older-than` option. Checkpoints exclude the database, sessions, memory, and logs.

## Memory migration

```bash
echo-agent migrate status
echo-agent migrate run --dry-run
echo-agent migrate run -y
echo-agent migrate run --adopt-empty --dry-run
echo-agent migrate memory-md
echo-agent migrate rollback
```

`migrate` only handles USER memory ownership and legacy `MEMORY.*.md` imports; it does not migrate SQLite schema. `--adopt-empty` only affects `run`. See [upgrade and data migration](../operations/upgrade-migrations.en.md).

## Skill dependencies

```bash
echo-agent deps status --json
echo-agent deps install FEATURE -y
echo-agent deps refresh
```

The `deps` parser passes remaining arguments to the dependency manager. Use `echo-agent deps --help` to inspect available features.

## Deprecated service alias

`service` retains old scripts' service actions but uses legacy Linux system-level semantics. Use `gateway <action>` in new scripts. The removal version is announced by the installed CLI warning.

## Exit codes

Successful commands return 0. Argument parsing errors generally return 2; other errors return nonzero status from the relevant subcommand. Scripts should check stderr or available `--json` output rather than assuming a universal numeric error-code map.
