# Filesystem Layout Reference

Echo Agent organizes data across global and workspace-level directories. This page documents every directory, file, and their purposes.

---

## Directory Hierarchy

### Global Directory (`~/.echo-agent/`)

The global directory stores user-wide configuration, data, and runtime state.

```
~/.echo-agent/
├── config.yaml              # Global configuration file
├── credentials.yaml         # Encrypted credentials store
├── profiles/                # Named configuration profiles
│   ├── personal.yaml
│   └── work.yaml
├── data/
│   ├── echo_agent.db        # Main SQLite database
│   ├── memory/              # Memory export and backup files
│   │   ├── episodic/        # Episodic memory segments
│   │   └── semantic/        # Semantic memory index
│   ├── knowledge/           # Knowledge base documents
│   │   ├── documents/       # Ingested source files
│   │   ├── index/           # Vector index files
│   │   └── metadata.json    # Index metadata
│   ├── spill/               # Large content overflow
│   │   └── *.spill          # Individual spill files
│   ├── logs/                # Audit trails and trace files
│   │   ├── tool_audit.jsonl # Tool invocation audit
│   │   └── memory_audit.jsonl
│   └── checkpoints/         # State checkpoints
│       └── <timestamp>/     # Individual checkpoint dirs
├── plugins/                 # Installed plugin packages
│   └── <plugin-name>/
│       ├── manifest.json
│       └── ...
├── skills/                  # Evolved and installed skills
│   ├── promoted/            # Production-ready skills
│   ├── staged/              # Awaiting approval
│   └── candidates/          # Evolution candidates
├── cache/                   # Temporary runtime cache
│   ├── models/              # Model response cache
│   ├── web/                 # Web fetch cache
│   └── media/               # Generated media cache
└── gateway.pid              # Gateway process PID file
```

### Workspace Directory (`.echo-agent/`)

Each project can have a local `.echo-agent/` directory for workspace-specific overrides.

```
.echo-agent/
├── config.yaml              # Workspace config overrides
├── knowledge/               # Project-specific knowledge
│   └── docs/                # Local documents for RAG
├── skills/                  # Project-specific skills
├── memory/                  # Workspace-scoped memory
└── .gitignore               # Excludes sensitive files
```

!!! tip "Version control"
    Add `.echo-agent/config.yaml` and `.echo-agent/knowledge/` to version control. Exclude `.echo-agent/memory/` and any credential files.

---

## File Descriptions

### Configuration Files

| File | Location | Purpose |
|------|----------|---------|
| `config.yaml` | Global / Workspace | Primary configuration |
| `credentials.yaml` | Global only | Encrypted API keys and tokens |
| `profiles/*.yaml` | Global only | Named configuration presets |
| `gateway.pid` | Global only | Running gateway process ID |

### Database Files

There is a single SQLite database, not one file per subsystem. Its path comes from `storage.database_path`, default `data/echo_agent.db`:

| File | Purpose | Typical Size |
|------|---------|--------------|
| `echo_agent.db` | Sessions, tasks, scheduling, cost, evolution | 10 MB - 1 GB |
| `echo_agent.db-wal` | WAL journal | Varies |
| `echo_agent.db-shm` | Shared memory | Varies |

!!! warning "Database locking"
    SQLite databases use WAL mode for concurrent reads. Only one Echo Agent instance should write to a given database at a time. Running multiple agents against the same global directory is unsupported.

### Log Files

| File | Description |
|------|-------------|
| `tool_audit.jsonl` | Tool invocation audit trail |
| `memory_audit.jsonl` | Memory read/write audit trail |
| `loop_freeze.log` | Watchdog dump written when the event loop stalls |

Logs are not rotated by size or date, and there are no compressed archives — see [Trace files](#trace-files) for the one bound that does apply.

---

## Precedence Rules

When the same setting exists at multiple levels, the following precedence applies (highest to lowest):

| Priority | Source | Example |
|----------|--------|---------|
| 1 (highest) | CLI runtime overrides | `--gateway-port 4000` |
| 2 | Environment variables | `ECHO_AGENT_GATEWAY__PORT=4000` |
| 3 | Workspace config | `.echo-agent/config.yaml` |
| 4 | Global user config | `~/.echo-agent/config.yaml` |
| 5 (lowest) | Package defaults | Built-in defaults |

For data directories, workspace-scoped data is used when it exists. Otherwise, the global directory is used.

---

## Platform-Specific Paths

| Platform | Global Directory | Notes |
|----------|-----------------|-------|
| Linux | `~/.echo-agent/` | `$HOME/.echo-agent/` |
| macOS | `~/.echo-agent/` | `$HOME/.echo-agent/` |
| Windows (WSL2) | `~/.echo-agent/` | Inside WSL filesystem |
| Windows (native) | `%USERPROFILE%\.echo-agent\` | Not recommended; use WSL2 |

!!! warning "Windows native paths"
    On native Windows, some tools (shell, process) have reduced functionality. WSL2 is strongly recommended for full feature support.

### Custom Base Directory

Override the global directory location:

```bash
# Via environment variable
export ECHO_AGENT_STORAGE__BASE_DIR="/opt/echo-agent/data"

# Via config
storage:
  base_dir: /opt/echo-agent/data
```

---

## Permissions and Ownership

### Recommended Permissions (Linux/macOS)

| Path | Mode | Rationale |
|------|------|-----------|
| `~/.echo-agent/` | `700` | User-only access |
| `config.yaml` | `600` | May contain sensitive settings |
| `credentials.yaml` | `600` | Contains encrypted secrets |
| `data/` | `700` | Database and runtime data |
| `data/echo_agent.db` | `600` | Sensitive data store |
| `cache/` | `700` | Temporary data |
| `plugins/` | `700` | Executable plugin code |

```bash
# Set correct permissions on fresh install
chmod 700 ~/.echo-agent
chmod 600 ~/.echo-agent/config.yaml
chmod 600 ~/.echo-agent/credentials.yaml
find ~/.echo-agent/data -type f -name "*.db" -exec chmod 600 {} \;
```

!!! danger "Never run as root"
    Echo Agent should never run as root. The gateway binds to an unprivileged port (default 58123) on loopback and does not require elevated permissions.

---

## Size Management

### Spill Directory

The spill directory stores large content that exceeds context window limits. Files are automatically cleaned based on age.

| Setting | Default | Description |
|---------|---------|-------------|
| `spill.max_total_mb` | `512` | Maximum total spill directory size |
| `spill.retention_days` | `7` | Delete spill artifacts older than this |
| `spill.sweep_interval_hours` | `6` | How often the sweeper runs |

Cleanup applies both rules: artifacts past `retention_days` go first, and if the directory still exceeds `max_total_mb` the oldest remaining artifacts are removed until it fits.

### Trace files

There is no size- or age-based log rotation, and no `observability.log_rotation` section. What is bounded is the number of trace files, capped by count:

```yaml
observability:
  log_level: INFO
  max_trace_files: 500   # oldest traces are pruned past this; <=0 disables pruning
```

### Checkpoint Pruning

Checkpoints can accumulate over time. Use the CLI to manage:

```bash
# List checkpoints with size
echo-agent checkpoint list

# Prune checkpoints older than 7 days
echo-agent checkpoint prune --older-than 7d

# Keep only the 10 most recent
echo-agent checkpoint prune --keep 10
```

### Database Maintenance

SQLite databases grow over time. Periodic VACUUM reduces file size:

```bash
# Manual vacuum (agent must be stopped)
sqlite3 ~/.echo-agent/data/echo_agent.db "VACUUM;"
```

!!! question "Maintainer confirmation needed"
    Is there a built-in `echo-agent db vacuum` or `echo-agent db optimize` command planned?

---

## Backup and Restore

### What to Back Up

| Priority | Path | Contains |
|----------|------|----------|
| Critical | `credentials.yaml` | API keys (encrypted) |
| Critical | `data/echo_agent.db` | Sessions, tasks, cost, evolution |
| High | `config.yaml` | Configuration |
| High | `data/memory/` | Agent memory store |
| High | `skills/promoted/` | Evolved skills |
| Medium | `data/knowledge/` | Knowledge base |
| Low | `cache/` | Regeneratable cache (skip) |
| Low | `data/spill/` | Temporary overflow (skip) |

### Backup Script

```bash
#!/bin/bash
BACKUP_DIR="$HOME/echo-agent-backup/$(date +%Y%m%d)"
mkdir -p "$BACKUP_DIR"

# Stop the agent first for consistent backup

# Copy critical files
cp ~/.echo-agent/config.yaml "$BACKUP_DIR/"
cp ~/.echo-agent/credentials.yaml "$BACKUP_DIR/"
cp ~/.echo-agent/data/echo_agent.db "$BACKUP_DIR/"
cp -r ~/.echo-agent/data/memory/ "$BACKUP_DIR/memory/"
cp -r ~/.echo-agent/skills/promoted/ "$BACKUP_DIR/skills/"
cp -r ~/.echo-agent/data/knowledge/ "$BACKUP_DIR/knowledge/"


echo "Backup complete: $BACKUP_DIR"
```

### Restore from Checkpoint

```bash
# List available checkpoints
echo-agent checkpoint list

# Restore a specific checkpoint
echo-agent checkpoint restore <checkpoint-id>
```

!!! tip "Automated backups"
    Use `echo-agent cron` to schedule periodic backups as a cron job within the agent itself.

---

## Workspace `.gitignore`

Recommended `.echo-agent/.gitignore` for workspace directories:

```gitignore
# Exclude sensitive and generated files
credentials.yaml
memory/
*.db
*.db-wal
*.db-shm
cache/
*.pid
*.log
```
