# Dashboard Usage

The Echo Agent Dashboard is a web-based management interface for monitoring system status, managing sessions and resources, viewing logs, and analyzing usage data.

!!! question "Maintainer Confirmation Required"
    The default listening port and authentication configuration (e.g., API Key, OAuth) must be confirmed by maintainers based on the deployment environment.

## Architecture Overview

The Dashboard uses a frontend-backend separation architecture:

- **Frontend**: Single Page Application (SPA) providing real-time status panels and operation interfaces
- **Backend API**: Located in the `gateway/api/` directory, responsible for data aggregation and permission validation
- **WebSocket**: Used for real-time streaming of logs, session states, and other live data

## Accessing the Dashboard

After starting Echo Agent, open a browser and navigate to:

```
http://<host>:<port>/dashboard
```

!!! question "Maintainer Confirmation Required"
    The default port number needs confirmation. It is typically `8080` or configurable via the Config page.

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

- Browse by tier (short-term, long-term, archive)
- Full-text search across memory content
- View memory metadata and associated sessions
- Manually create or delete memory entries

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

- Upload and index documents
- Browse knowledge base entries
- Manage document categories and tags
- View indexing status and vectorization progress

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
- Create, edit, and archive tasks
- Filter by priority and tags
- View task associations with sessions and memory

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

!!! warning "Security Notice"
    The Dashboard should not be exposed to the public internet by default. It is recommended to protect access using a reverse proxy with authentication middleware, or restrict the listening address to `127.0.0.1`.

!!! question "Maintainer Confirmation Required"
    The specific authentication method (Basic Auth, Token, OAuth2) must be determined based on the deployment plan.

## Monitoring and Alerts

Through the Overview and Analytics pages, you can:

- Monitor system health status in real time
- Configure resource usage alert thresholds
- Monitor model cost budgets
- Detect abnormal sessions automatically
