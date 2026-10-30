# Backup and Restore

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

The default workspace is `~/.echo-agent`, but `workspace`, `-w/--workspace`, and individual `storage.*` settings can move data. Use `echo-agent config dump` to identify the effective paths. A full backup should cover configuration, the SQLite database, `storage.sessions_dir`, `storage.memory_dir`, and any knowledge documents or skills you need to retain. `checkpoint.store_path` defaults to `~/.echo-agent/checkpoints/store` and can be included as needed.

## Stopped-service full backup

Stop writes first. For an installed user-level background service:

```bash
echo-agent gateway stop
tar -czf "$HOME/echo-agent-backup-$(date +%Y%m%d).tar.gz" -C "$HOME" .echo-agent
echo-agent gateway start
```

For a foreground `echo-agent run` or `echo-agent gateway` process, stop that process first. Change the `-C` directory and archive member if you use a custom workspace. Do not copy a live SQLite database file directly. Backups may contain tokens, conversations, and memory; restrict access to the archive.

## Online SQLite backup

The SQLite CLI's `.backup` command makes a consistent database copy:

```bash
sqlite3 "$HOME/.echo-agent/data/echo_agent.db"   ".backup '$HOME/echo-agent-sqlite-backup.db'"
```

This copies only the database. Back up memory, sessions, configuration, and other files separately. Their timestamps may not match the database snapshot, so use a stopped-service backup for a consistent full-workspace archive.

## Restore a full backup

Stop the service or foreground process and preserve the current workspace before restoring an archive compatible with the target program version. For example:

```bash
echo-agent gateway stop
mv "$HOME/.echo-agent" "$HOME/.echo-agent.before-restore"
tar -xzf "$HOME/echo-agent-backup-20261001.tar.gz" -C "$HOME"
sqlite3 "$HOME/.echo-agent/data/echo_agent.db" "PRAGMA integrity_check;"
echo-agent config validate
echo-agent gateway start
echo-agent gateway status
```

Replace paths and filenames with your actual backup. Inspect the archive and its source before restoring. If no background service is installed, stop and restart the corresponding foreground process instead.

## Restore an SQLite backup { #restore-sqlite-backup }

A database-only restore must be compatible with the remaining files:

```bash
echo-agent gateway stop
cp "$HOME/echo-agent-sqlite-backup.db" "$HOME/.echo-agent/data/echo_agent.db"
sqlite3 "$HOME/.echo-agent/data/echo_agent.db" "PRAGMA integrity_check;"
echo-agent gateway start
```

Preserve the current database before overwriting it. Restore sessions, memory, and configuration from matching backups if they are stored outside the copied database.

## Checkpoint scope

`echo-agent checkpoint list`, `show`, `restore`, and `prune` operate on shadow Git snapshots of workspace files. Snapshots exclude the database, sessions, memory, and logs. They are not full backups and cannot reverse SQLite schema changes. See the [filesystem layout](../reference/filesystem-layout.en.md) and [upgrade and data migration](upgrade-migrations.en.md).
