# Dashboard Usage

The Echo Agent Dashboard is a web-based management interface for monitoring system status, managing sessions and resources, viewing logs, and analyzing usage data.

The Dashboard is served by the gateway process rather than on a port of its own; both its address and its authentication come from the `gateway` configuration.

## Architecture Overview

The Dashboard uses a frontend-backend separation architecture:

- **Frontend**: Single Page Application (SPA) providing real-time status panels and operation interfaces
- **Backend API**: Located in the `gateway/api/` directory, responsible for data aggregation and permission validation
- **WebSocket**: Used for real-time streaming of logs, session states, and other live data

## Accessing the Dashboard

Start the gateway (`echo-agent gateway`), then open the gateway address in a browser:

```
http://127.0.0.1:58123/
```

The Dashboard is a single-page application mounted at the gateway root. Its port is `gateway.port` (`58123` by default; set it to `0` for a dynamically assigned port, which is written to `workspace/.echo-agent/gateway.json`). When the frontend bundle has not been built, that address falls back to the built-in playground page — run `echo-agent dashboard build` to produce the bundle.

## Navigation Guide

The left sidebar contains the main navigation with the following pages:

| Page | Function |
|------|----------|
| Overview | System overview and health status |
| Sessions | Session management |
| Memory | Memory storage browsing |
| Skills | Skills management |
| Knowledge | Knowledge base management |
| Cron | Scheduled job management |
| Kanban | Task board visualization |
| Logs | System logs |
| Analytics | Usage statistics |
| Config | Runtime configuration |
| Channels | Channel configuration |

---

## Page Details

### Overview

Displays the overall system health dashboard.

**Key Features:**

- Active session count
- System resource usage (CPU, memory)
- Channel connection status
- Recent events timeline

<!-- Screenshot placeholder: Overview page showing status cards and resource charts -->

### Sessions

View and manage all active and historical sessions.

**Key Features:**

- View active session list and details
- Browse historical session records
- Manually terminate abnormal sessions
- Filter sessions by channel and time range

<!-- Screenshot placeholder: Sessions list showing status, source channel, and duration -->

### Memory

Browse and search memory entries across all tiers.

**Key Features:**

- Filter by the four tiers: `working`, `episodic`, `semantic`, `archival`
- Search memory content
- View memory metadata
- Delete memory entries (the page offers no manual create action)

This page is a cross-subject view: both the listing and the search require an admin token, since a regular API token can only read memories within its own scope.

<!-- Screenshot placeholder: Memory search interface with entry cards and tier filters -->

### Skills

Manage installed skills and their configurations.

**Key Features:**

- View installed skills list
- Enable/disable individual skills
- View skill trigger conditions and descriptions
- Skill execution logs

<!-- Screenshot placeholder: Skills page showing skill cards grid with toggle switches -->

### Knowledge

Manage documents and knowledge base content.

**Key Features:**

- Upload documents and trigger an index rebuild
- Browse indexed documents (path, size, modification time)
- View index status: document count, chunk count, staleness, last rebuild time

Upload, delete and rebuild are all guarded server-side by the admin guard. The page has no document category or tag features.

<!-- Screenshot placeholder: Knowledge page showing document list and indexing status indicators -->

### Cron

Configure and monitor scheduled tasks.

**Key Features:**

- Create/edit/delete scheduled jobs
- View cron expressions and next execution times
- Manually trigger job execution
- View execution history and results

<!-- Screenshot placeholder: Cron page showing job table and execution timeline -->

### Kanban

Visualize and manage tasks using a board layout.

**Key Features:**

- Drag and drop task cards to change status
- Toggle visibility of the archived columns (`cancelled`, `suspended`)

Dragging requires write access: under a read-only token the board renders read-only, rather than offering controls that answer 403 on every drag.

<!-- Screenshot placeholder: Kanban page showing multi-column board with task cards -->

### Logs

View system logs in real time.

**Key Features:**

- Real-time log streaming (via WebSocket)
- Filter by level (DEBUG / INFO / WARN / ERROR)
- Filter by module and time range
- Log export

<!-- Screenshot placeholder: Logs page showing log stream and level filter controls -->

### Analytics

View usage statistics and model invocation costs.

**Key Features:**

- Model invocation count and token consumption trends
- Cost breakdown by model
- Usage distribution by channel and skill
- Custom time range reports

<!-- Screenshot placeholder: Analytics page showing line charts, pie charts, and cost summary table -->

### Config

Manage runtime system parameters without restarting the service.

**Key Features:**

- Edit model provider configuration (API keys, endpoints)
- Adjust memory strategy parameters
- Modify log levels
- Manage environment variable overrides
- Configuration change history and rollback

!!! warning "Caution"
    Changes made on the Config page take effect immediately. Proceed carefully. It is recommended to make configuration changes during off-peak hours.

<!-- Screenshot placeholder: Config page showing configuration form and change history panel -->

### Channels

Configure messaging channel connection parameters.

**Key Features:**

- Add/edit/delete channels
- Configure channel authentication credentials
- View channel connection status and health checks
- Channel message throughput monitoring

<!-- Screenshot placeholder: Channels page showing channel list and connection status indicators -->

---

## Access Control

The Dashboard inherits the gateway's authentication; it has no authentication settings of its own. `gateway.host` defaults to `127.0.0.1`, so only the local machine is served.

With no additional configuration (`gateway.auth.mode` at `allowlist` and an empty list), browser requests carrying a cross-site `Origin` are rejected, which means the Dashboard page cannot call the API in that state. To open browser access, set `gateway.auth.mode` to `open`, add the user to `gateway.auth.allowed_users`, or add the origin to `gateway.auth.allowed_origins`.

!!! warning "Before exposing it publicly"
    Configure authentication and TLS termination at a reverse proxy before changing `gateway.host` to a non-loopback address. See [gateway authentication](../integrations/gateway/authentication.md) and [security hardening](../operations/security-hardening.md).

## Monitoring and Alerts

Through the Overview and Analytics pages, you can:

- Monitor system health status in real time
- Configure resource usage alert thresholds
- Monitor model cost budgets
- Detect abnormal sessions automatically
