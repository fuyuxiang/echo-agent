# Backup & Restore

Protect your Echo Agent data with regular backups.

---

## What to Back Up

| Data | Location | Priority |
|------|----------|----------|
| Configuration | `~/.echo-agent/config.yaml` | High |
| SQLite database | `~/.echo-agent/data/` | High |
| Memory store | `~/.echo-agent/data/memory.db` | High |
| Knowledge base | `~/.echo-agent/data/knowledge/` | Medium |
| Skills (user) | `~/.echo-agent/skills/` | Medium |
| Checkpoints | `~/.echo-agent/data/checkpoints/` | Low |
| Logs | `~/.echo-agent/logs/` | Low |

## Backup Procedure

```bash
# Stop the service first for consistency
echo-agent gateway stop

# Create backup
BACKUP_DIR="echo-agent-backup-$(date +%Y%m%d)"
mkdir -p "$BACKUP_DIR"
cp -r ~/.echo-agent/config.yaml "$BACKUP_DIR/"
cp -r ~/.echo-agent/data/ "$BACKUP_DIR/"
cp -r ~/.echo-agent/skills/ "$BACKUP_DIR/"

# Restart
echo-agent gateway start
```

!!! warning
    Always stop the Gateway before backing up SQLite databases to avoid corruption.

## Restore

```bash
echo-agent gateway stop
cp -r "$BACKUP_DIR/data/" ~/.echo-agent/
cp "$BACKUP_DIR/config.yaml" ~/.echo-agent/
echo-agent gateway start
```

## Checkpoint System

Echo Agent provides built-in file checkpoints:

```bash
echo-agent checkpoint list
echo-agent checkpoint show <id>
echo-agent checkpoint restore <id>
```

Checkpoints track file-level changes made by the agent and allow targeted rollback.
