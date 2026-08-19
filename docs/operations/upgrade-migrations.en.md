# Upgrade & Migrations

Safely upgrade Echo Agent and migrate data between versions.

---

## Upgrade Process

```bash
# 1. Check current version
echo-agent --version

# 2. Stop the service
echo-agent gateway stop

# 3. Back up data
cp -r ~/.echo-agent ~/.echo-agent.bak

# 4. Upgrade
pip install --upgrade echo-agent

# 5. Run migrations
echo-agent migrate status
echo-agent migrate run

# 6. Restart
echo-agent gateway start

# 7. Verify
echo-agent status
```

## Migration Commands

```bash
# Check pending migrations
echo-agent migrate status

# Run all pending migrations
echo-agent migrate run

# Dry-run (preview only)
echo-agent migrate run --dry-run

# Rollback last migration
echo-agent migrate rollback

# Migrate memory.md format (legacy)
echo-agent migrate memory-md
```

## Configuration Migration

Configuration fields may be renamed or restructured between versions. Echo Agent automatically migrates known field changes during config loading.

Deprecated fields generate warnings:

```
WARNING: 'service' command is deprecated, use 'gateway <action>' instead
```

## Rollback

If issues arise after upgrade:

```bash
echo-agent gateway stop
pip install echo-agent==0.3.6  # previous version
cp -r ~/.echo-agent.bak/* ~/.echo-agent/
echo-agent migrate rollback
echo-agent gateway start
```

!!! question "Maintainer Decision Required"
    Formal backward compatibility and database downgrade guarantees are not yet defined.
