# Filesystem Layout

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

`Config.workspace` defaults to `~/.echo-agent`. Supported CLI subcommands can override it with `-w/--workspace`. Relative paths below are based on the effective workspace; configuration can move some of them. Use `echo-agent config dump` to inspect the effective paths.

## Main workspace paths

| Path or setting | Default | Contents |
|---|---|---|
| `storage.database_path` | `data/echo_agent.db` | SQLite database; SQLite may create `-wal` and `-shm` sidecars |
| `storage.sessions_dir` | `data/sessions` | Session data |
| `storage.memory_dir` | `data/memory` | Memory files such as `user_memory.json` and `env_memory.json`; shards may use `MEMORY.*.md` |
| `knowledge.docs_dir` | `data/knowledge` | Source documents for the knowledge base |
| `storage.spill_dir` | `data/spill` | Oversized tool output; must be a dedicated workspace-relative directory |
| `storage.logs_dir` | `data/logs` | Execution traces, tool and memory audits, watchdog dumps; ordinary Loguru logs go to stderr |
| `artifacts.root_dir` | `data/artifacts` | User-deliverable artifacts |
| `skills.skills_dir` | `skills` | Workspace skills |

Not every directory is created at first launch. A skill is described by `SKILL.md`; do not assume each one has a `handler.py`, `manifest.yaml`, or test directory.

## Configuration files

The loader looks for `echo-agent.yaml`, `echo-agent.yml`, `config.yaml`, and `config.yml` in that order within its search directories. Without an explicit file, it searches the current and user directories; `-c/--config` selects a file. See [environment variables](environment-variables.en.md) for configuration precedence. `.echo-agent/config.yaml` is not an automatically applied, separate workspace-override layer.

Before checking configuration into version control, ensure it contains no plaintext secrets. Runtime databases, sessions, memory, and audit records generally belong outside the repository.

## Checkpoints

`checkpoint.store_path` defaults to `~/.echo-agent/checkpoints/store`. Checkpoints live in a shadow Git repository, not a `chk_<timestamp>/manifest.json` tree. Workspace snapshots exclude the database, sessions, memory, logs, and the checkpoint store itself; `checkpoint restore` cannot recover those data. Current CLI actions are `echo-agent checkpoint list`, `show`, `restore`, and `prune`. `prune` has no `--older-than` or `--keep` option.

## Backup and cleanup

A full backup should include the actual workspace's database, sessions, memory, configuration, and other important data. Stop writes first, or use a consistent SQLite online backup method. Schedule database maintenance such as `VACUUM` during a suitable downtime or low-load window. See [backup and restore](../operations/backup-restore.en.md).
