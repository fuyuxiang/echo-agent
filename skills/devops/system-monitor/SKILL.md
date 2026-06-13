---
name: system-monitor
description: "Monitor system health: CPU, memory, disk, processes, and network. Alert on thresholds via any channel."
version: 1.0.0
metadata:
  echo:
    tags: [Monitor, System, CPU, Memory, Disk, DevOps]
---

# System Monitor

System health monitoring with configurable alert thresholds.

## Quick Status

```bash
# macOS
top -l 1 | head -10
df -h | grep -v tmpfs

# Linux
free -h && uptime
df -h | grep -v tmpfs
```

## Script (psutil-based)

```bash
pip install psutil

python3 scripts/system_check.py status       # full report
python3 scripts/system_check.py cpu           # CPU only
python3 scripts/system_check.py memory        # Memory only
python3 scripts/system_check.py disk          # Disk only
python3 scripts/system_check.py processes     # top consumers
python3 scripts/system_check.py alerts        # check thresholds
```

## Output Example

```
System Status Report — 2026-06-13 14:30
CPU: 23.5% (8 cores)
Memory: 12.4/16.0 GB (77.5%)
Disk /: 180/500 GB (36.0%)
Load: 2.1 / 1.8 / 1.5
Uptime: 14 days, 3:22:10

Top Processes (CPU):
  1. python3 (pid 1234) — 15.2% CPU, 1.2 GB RAM
  2. node (pid 5678) — 8.1% CPU, 800 MB RAM
```

## Alert Configuration

`~/.echo-agent/monitor.yaml`:

```yaml
thresholds:
  cpu_percent: 90
  memory_percent: 85
  disk_percent: 90
  load_average_5m: 8.0
check_interval: "*/5 * * * *"
alert_channel: telegram
```

## Integration

Schedule with cron channel (`*/5 * * * *`) to check every 5 minutes. When thresholds are exceeded, alert is sent through configured channel.

## Monitoring Stack (Optional)

For persistent monitoring with dashboards, consider:
- Prometheus + Grafana (heavy but powerful)
- Glances (`pip install glances`) — terminal dashboard
- psutil + SQLite for historical data
