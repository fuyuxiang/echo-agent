# Scheduled Jobs

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

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
| `max_concurrent_jobs` | `10` | Capacity covers the full run; excess manual triggers return a conflict and scheduled checks retry later |

## Creating Scheduled Jobs

Use the cronjob tool to create a job:

```yaml
tool: cronjob
action: create
name: "daily-report"
schedule: "0 9 * * *"
command: "Generate daily summary report and send to notification channel"
```

Cron uses the server local timezone. Fix the deployment timezone and inspect the next occurrence. Day/month fields in a normal five-field expression recur and are not a one-shot date reminder.

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

Interactive `cronjob create/authorize` requires human approval of a dangerous action, bound to the displayed instruction, schedule and delivery target. An administrator may explicitly set `authorize_unattended: true` through Dashboard/REST. `cron_authorized` is an internal fact derived by the scheduler from an existing grant, not a caller-supplied authorization mechanism.

Authorization is granted **per job**; there is no channel-level auto-authorization rule. A newly created job starts unauthorized and does not inherit permission to run simply by belonging to the cron channel.

The split is deliberate: scheduling and permission are separate concerns. A job's schedule can be edited freely, but whether it may act unsupervised takes one explicit human confirmation.

#### Re-authorizing an existing job

A grant is bound to the job's content, so **editing the instruction, schedule or
delivery target invalidates it**. Re-authorizing is therefore routine rather than
exceptional. There are three paths:

| Path | Works while running | Notes |
|------|:---:|------|
| Say "authorize scheduled job `<job_id>`" in chat | ✅ | The agent calls `cronjob(action="authorize")`, which first shows the job's instruction, schedule and delivery target for confirmation |
| Tick the authorization box on the Dashboard cron page | ✅ | Equivalent to REST `PUT /cron/{id}` with `authorize_unattended: true` |
| `echo-agent cron authorize <job_id>` | ❌ | Only with the **service stopped** |

The CLI path is guarded by the instance lock: it refuses outright while the
gateway is running, because an offline edit would be overwritten by the live
instance. Use chat or the Dashboard while the service is up.

```bash
# These require the service to be stopped first
echo-agent cron list                  # list jobs and their authorization state
echo-agent cron authorize <job_id>    # authorize one job
echo-agent cron revoke <job_id>       # revoke it
```

Revoking works from chat too: say "revoke authorization for scheduled job
`<job_id>`" (`cronjob(action="revoke")`).

### Unattended Mode

When `unattended=true`, the approval flow differs:

- An authorized job may execute permitted WRITE/EXEC operations
- DANGEROUS operations, including creation of new scheduled jobs, remain denied; unattended work cannot self-escalate or wait indefinitely for an absent approver

## Cron Channel

Scheduled CRON events use the same Runtime. When a source session or delivery target is provided, results go there. An unresolved target uses `cron:<job_id>` and reports an undeliverable result instead of treating dropped output as success.

Each occurrence persists a unique `run_id` before enqueueing. `queued` means admission only; `completed` means the turn finished and delivery handling succeeded, with remote confirmation depending on the channel's receipt capability. Unfinished turns are `incomplete`; delivery failures are `error`. The latest 100 occurrences are retained, and late writebacks can only update their own occurrence.

The same job cannot overlap itself, including manual triggers. On restart, prior queued/executing occurrences become `interrupted` and are not blindly replayed. Inspect prior effects before manually retrying. Recurring schedules are recalculated from now rather than replaying every missed occurrence.

### Delivery recovery and report history

Missing or rejected Weixin conversation context leaves saved results in `delivery_pending` and
releases the execution slot. Fresh inbound activity resumes the saved delivery; success is recorded
as `delivery_recovered` with its recovery time. The agent and its tools are not re-executed. Unknown
delivery outcomes require verification in the dashboard before retrying.

An optional payload field `report_history_path`, such as `data/ai-radar/history.json`, connects a
workspace-relative JSON report journal. The runtime maintains delivery fields in its `reports` array,
keyed by execution date: `pending_delivery` until complete receipts arrive, then `status: success`
and the actual `pushed_at`. `job_id`, `run_id` and `delivery_status` link to scheduler records.
Research content remains task-managed; premature same-day `last_pushed` values are withdrawn on
delivery failure. Changing this path invalidates the existing unattended authorization.

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

### Pause and resume

Use Dashboard Cron or administrator `PUT {api_prefix}/cron/{id}` to change `enabled`. The `cronjob` tool accepts only create/list/delete/trigger/authorize/revoke, not pause/resume. Pausing does not undo prior side effects; inspect the active run/history when a new trigger conflicts.

### Delete a Job

```yaml
tool: cronjob
action: delete
job_id: "<id returned by create/list>"
```

## Use Case Examples

### Daily Report Generation

```yaml
tool: cronjob
action: create
name: "daily-summary"
schedule: "0 9 * * *"
command: "Summarize the past 24 hours of channel activity and generate a report"
```

### Periodic Cleanup

```yaml
tool: cronjob
action: create
name: "weekly-cleanup"
schedule: "0 3 * * 0"
command: "Clean up temporary files and expired caches older than 30 days"
```

### Health Check

```yaml
tool: cronjob
action: create
name: "health-check"
schedule: "*/30 * * * *"
command: "Check connectivity to all backend services, send alert on failure"
```

### Data Synchronization

```yaml
tool: cronjob
action: create
name: "sync-external-data"
schedule: "0 */4 * * *"
command: "Sync latest data from external API to local storage"
```

## Security Recommendations

!!! warning "Principle of Least Privilege"
    Scheduled jobs should only be granted the minimum permissions needed to accomplish their function. Avoid creating scheduled jobs with broad permissions.

- Regularly review the list of active scheduled jobs
- Set execution time windows for jobs that perform sensitive operations
- Monitor `max_concurrent_jobs` usage to prevent resource exhaustion
- Use the `cron_authorized` pre-authorization flag cautiously in production environments
