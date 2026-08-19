# Scheduled Jobs

Echo Agent includes a built-in scheduling system that allows you to create, manage, and monitor periodic tasks using the cronjob tool. This guide covers the complete usage of the scheduling system.

## System Overview

The scheduling system consists of the following components:

- **cronjob tool** — Core tool for creating and managing scheduled jobs
- **Scheduler** — Triggers job execution based on cron expressions
- **Cron Channel** — Dedicated channel that carries job output and status
- **Dashboard Cron page** — Visual management interface

## Scheduler Configuration

The scheduler is configured globally via `SchedulerConfig`:

```yaml
scheduler:
  enabled: true
  max_concurrent_jobs: 10
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `enabled` | `true` | Whether the scheduling system is active |
| `max_concurrent_jobs` | `10` | Maximum concurrent jobs; excess jobs are queued |

## Creating Scheduled Jobs

Use the cronjob tool to create a job:

```yaml
tool: cronjob
action: create
name: "daily-report"
schedule: "0 9 * * *"
task: "Generate daily summary report and send to notification channel"
```

### Cron Expression Syntax

Standard five-field cron format:

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day of month (1-31)
│ │ │ ┌───────────── month (1-12)
│ │ │ │ ┌───────────── day of week (0-6, 0=Sunday)
│ │ │ │ │
* * * * *
```

Common examples:

| Expression | Meaning |
|------------|---------|
| `0 9 * * *` | Every day at 9:00 |
| `*/15 * * * *` | Every 15 minutes |
| `0 0 * * 1` | Every Monday at 00:00 |
| `0 8 1 * *` | 1st of every month at 8:00 |
| `0 */2 * * *` | Every 2 hours |

## Authorization Model

!!! danger "Security Warning"
    The cronjob tool has a risk level of `dangerous`. Creating new scheduled jobs requires explicit authorization approval.

### Why "dangerous" Risk Level

Scheduled jobs run periodically in an unattended manner, which means they can:

- Consume significant system resources
- Execute sensitive operations
- Produce unexpected side effects

### Approval Flow

Creating a new job requires one of the following:

1. **Human approval** (`approval_source="human"`) — Maintainer confirms via Dashboard or interaction
2. **Pre-authorization flag** (`cron_authorized=true`) — Set in `ToolExecutionContext`

```yaml
# ToolExecutionContext example
context:
  cron_authorized: true
  unattended: false
```

!!! question "Needs maintainer confirmation"
    The detailed authorization flow for Cron Channel (such as channel-level auto-authorization rules) needs maintainer confirmation on the specific implementation.

### Unattended Mode

When `unattended=true`, the approval flow differs:

- If `cron_authorized=true` is also set, jobs can be created automatically
- If `cron_authorized` is not set, job creation is rejected (it will not hang waiting for human approval)

## Cron Channel

The Cron Channel is a dedicated execution environment for scheduled jobs:

- Each scheduled job is bound to a cron channel
- Job output and status information is written to the channel
- The channel provides an isolated context for job execution

## Dashboard Cron Page

The Dashboard provides a dedicated Cron management page that supports:

- Viewing all scheduled jobs and their statuses
- Manually triggering job execution
- Pausing/resuming jobs
- Viewing job execution history and logs
- Deleting jobs

## Managing Jobs

### List Jobs

```yaml
tool: cronjob
action: list
```

### Pause a Job

```yaml
tool: cronjob
action: pause
name: "daily-report"
```

### Resume a Job

```yaml
tool: cronjob
action: resume
name: "daily-report"
```

### Delete a Job

```yaml
tool: cronjob
action: delete
name: "daily-report"
```

## Use Case Examples

### Daily Report Generation

```yaml
tool: cronjob
action: create
name: "daily-summary"
schedule: "0 9 * * *"
task: "Summarize the past 24 hours of channel activity and generate a report"
```

### Periodic Cleanup

```yaml
tool: cronjob
action: create
name: "weekly-cleanup"
schedule: "0 3 * * 0"
task: "Clean up temporary files and expired caches older than 30 days"
```

### Health Check

```yaml
tool: cronjob
action: create
name: "health-check"
schedule: "*/30 * * * *"
task: "Check connectivity to all backend services, send alert on failure"
```

### Data Synchronization

```yaml
tool: cronjob
action: create
name: "sync-external-data"
schedule: "0 */4 * * *"
task: "Sync latest data from external API to local storage"
```

## Security Recommendations

!!! warning "Principle of Least Privilege"
    Scheduled jobs should only be granted the minimum permissions needed to accomplish their function. Avoid creating scheduled jobs with broad permissions.

- Regularly review the list of active scheduled jobs
- Set execution time windows for jobs that perform sensitive operations
- Monitor `max_concurrent_jobs` usage to prevent resource exhaustion
- Use the `cron_authorized` pre-authorization flag cautiously in production environments
