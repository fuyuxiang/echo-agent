# Upgrade & Uninstall

## Upgrade

=== "pip Upgrade"

    ```bash
    pip install --upgrade echo-agent[all]
    ```

    Upgrade to a specific version:

    ```bash
    pip install "echo-agent[all]==0.3.8"
    ```

=== "Source Upgrade"

    ```bash
    cd echo-agent
    git pull origin master
    cd echo-agent-runtime
    (cd web && pnpm install --frozen-lockfile && pnpm build)
    pip install -e ".[all]"
    ```

For separately downloaded Runtime sources or release archives, preserve the workspace/config, replace sources and reinstall. Extracted directories without `.git` do not use `git pull`. Official release archives include the Dashboard and skip the frontend build.

---

### Pre-upgrade Checklist

!!! warning "Upgrade Notes"
    Breaking changes may occur between Beta versions. Before upgrading:

    1. Read the [CHANGELOG](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/CHANGELOG.md) for details on changes
    2. Back up your data directory
    3. Stop the service, back up the full workspace, then upgrade and validate the configuration

**Back up data:**

```bash
# Default data directory location
tar -czf "$HOME/echo-agent-backup-$(date +%Y%m%d).tar.gz" -C "$HOME" .echo-agent
```

---

### Data migration

SQLite schema migrations run automatically during database initialization. `echo-agent migrate` only updates USER memory ownership keys or imports legacy memory shards; it does not migrate the database schema. For a memory migration, first run `echo-agent migrate status` and `echo-agent migrate run --dry-run`. See [upgrade and data migration](../operations/upgrade-migrations.en.md).

---

### Checkpoint Recovery

Checkpoints can restore workspace files changed by the agent:

```bash
# List available checkpoints
echo-agent checkpoint list

# Restore a specific checkpoint
echo-agent checkpoint restore <checkpoint-id>
```

---

## Uninstall

### Package Only

```bash
pip uninstall echo-agent
```

### Full Cleanup

Stop and uninstall the Gateway background service first. After backing up required data, uninstall the package and remove the default workspace:

```bash
echo-agent gateway stop
echo-agent gateway uninstall
pip uninstall echo-agent
rm -rf ~/.echo-agent
rm -f ~/.local/bin/echo-agent
```

The one-line installer puts its source checkout and virtual environment under `~/.echo-agent/echo-agent/venv` by default, so removing the default workspace removes them as well. If you used a custom `ECHO_INSTALL_DIR`, inspect that directory before handling it separately. `pip uninstall` only affects the active Python environment.

!!! warning "Data is Unrecoverable"
    Deleting `~/.echo-agent` permanently removes all data, including:

    - Configuration file (`config.yaml`)
    - SQLite database, session files, and memory files
    - Accumulated skills and evolution records
    - Scheduled task configurations

    Make sure to back up important data before deletion.

---

### Clean Up Playwright Browsers

If Playwright browser dependencies were installed:

Playwright browser caches may be shared with other projects. Check the management commands and cache location for your installed version before removing them.

---

### Clean Up Frontend Build Artifacts

If you installed from source and built the frontend:

```bash
cd echo-agent/echo-agent-runtime/web
rm -rf node_modules dist
```

---

## Downgrade

If a new version has issues and you need to roll back:

```bash
pip install "echo-agent[all]==0.3.6"
```

!!! warning "Checkpoints do not contain the database"
    `echo-agent checkpoint` is a shadow Git snapshot of workspace **files**. It excludes the SQLite database, sessions, memory, and logs; `checkpoint restore` cannot recover them.

    Downgrading the package does not reverse database changes. Stop the service and restore a full backup compatible with the older version. `echo-agent migrate rollback` only restores the latest USER memory migration backup. See [upgrade and data migration](../operations/upgrade-migrations.en.md) and [backup and restore](../operations/backup-restore.en.md).
