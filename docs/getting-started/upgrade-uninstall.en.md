# Upgrade & Uninstall

## Upgrade

=== "pip Upgrade"

    ```bash
    pip install --upgrade echo-agent[all]
    ```

    Upgrade to a specific version:

    ```bash
    pip install echo-agent[all]==0.3.7
    ```

=== "Source Upgrade"

    ```bash
    cd echo-agent
    git pull origin master
    pip install -e ".[all]"
    ```

---

### Pre-upgrade Checklist

!!! warning "Upgrade Notes"
    Breaking changes may occur between Beta versions. Before upgrading:

    1. Read the [CHANGELOG](https://github.com/fuyuxiang/echo-agent/blob/master/CHANGELOG.md) for details on changes
    2. Back up your data directory
    3. Run database migration if required

**Back up data:**

```bash
# Default data directory location
cp -r ~/.echo-agent ~/.echo-agent.backup.$(date +%Y%m%d)
```

---

### Database Migration

If the database schema has changed between versions, run the migration command after upgrading:

```bash
echo-agent migrate
```

!!! note "Automatic Migration Detection"
    `echo-agent run` checks the schema version on startup. If migration is needed, it displays a prompt and refuses to start — run `echo-agent migrate` to resolve.

---

### Checkpoint Recovery

If issues arise after upgrading, roll back to a previous checkpoint:

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

Uninstall the package and remove all data:

```bash
# Uninstall Python package
pip uninstall echo-agent

# Remove data directory (config, database, memory)
rm -rf ~/.echo-agent

# If installed via the one-line script, also remove the venv
rm -rf ~/.echo-agent/venv
rm -f ~/.local/bin/echo-agent
```

!!! warning "Data is Unrecoverable"
    Deleting `~/.echo-agent` permanently removes all data, including:

    - Configuration file (`config.yaml`)
    - Conversation history and memory database
    - Accumulated skills and evolution records
    - Scheduled task configurations

    Make sure to back up important data before deletion.

---

### Clean Up Playwright Browsers

If Playwright browser dependencies were installed:

```bash
# List installed browsers
playwright install --list

# Remove all Playwright browsers
rm -rf ~/.cache/ms-playwright        # Linux
rm -rf ~/Library/Caches/ms-playwright # macOS
```

---

### Clean Up Frontend Build Artifacts

If you installed from source and built the frontend:

```bash
cd echo-agent/web
rm -rf node_modules dist
```

---

## Downgrade

If a new version has issues and you need to roll back:

```bash
# Install a specific older version
Install the specific older version:

```bash
pip install echo-agent[all]==0.3.6
```

If `echo-agent migrate run` was applied during the upgrade, undo it with the matching command:

```bash
echo-agent migrate rollback
```

!!! warning "Checkpoints do not contain the database"
    `echo-agent checkpoint` is a shadow Git snapshot of workspace **files**, and its exclusion list explicitly covers the SQLite database, the sessions directory, the memory directory and the logs directory (a file-level snapshot of a live SQLite file would be a torn read). `checkpoint restore` therefore cannot restore database state.

    Data-layer rollback has exactly two routes: `echo-agent migrate rollback`, or a database file you copied yourself beforehand. Copy `data/echo_agent.db` before downgrading.
