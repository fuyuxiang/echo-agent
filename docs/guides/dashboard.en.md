# Dashboard Usage

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

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

The simplified page is also available at `/playground`. It discovers API and interactive WS paths through `/meta`. Enter an existing API/admin token when authentication is enabled; the page sends it in headers or the WS auth frame, never in a URL or browser storage. Release wheels include the Dashboard and need no Node/pnpm at runtime; those tools are needed to build from source.

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
- Memory entries, running tasks and interactive WS clients
- Channel connection status
- Event-driven refresh with polling as a reconnect backstop


### Sessions

View and manage all active and historical sessions.

**Key Features:**

- View active session list and details
- Browse historical session records
- Reset a conversation; page through history and inspect execution records
- Search session identifiers


### Memory

Browse and search memory entries across all tiers.

**Key Features:**

- Filter by the four tiers: `working`, `episodic`, `semantic`, `archival`
- Search memory content
- View memory metadata
- Delete memory entries (the page offers no manual create action)

This page is a cross-subject view: both the listing and the search require an admin token, since a regular API token can only read memories within its own scope.


### Skills

Manage installed skills and their configurations.

**Key Features:**

- View installed skills list
- Enable/disable individual skills
- View skill trigger conditions and descriptions
- Read skill content, files and dependency status, and install declared dependencies


### Knowledge

Manage documents and knowledge base content.

**Key Features:**

- Upload documents and trigger an index rebuild
- Browse indexed documents (path, size, modification time)
- View index status: document count, chunk count, staleness, last rebuild time

Upload, delete and rebuild are all guarded server-side by the admin guard. The page has no document category or tag features.


### Cron

Configure and monitor scheduled tasks.

**Key Features:**

- Create/edit/delete scheduled jobs
- View cron expressions and next execution times
- Manually trigger job execution
- View execution history and results


### Kanban

Visualize and manage tasks using a board layout.

**Key Features:**

- Drag and drop task cards to change status
- Toggle visibility of the archived columns (`cancelled`, `suspended`)

Dragging requires write access: under a read-only token the board renders read-only, rather than offering controls that answer 403 on every drag.


### Logs

View system logs in real time.

**Key Features:**

- Real-time log streaming (via WebSocket)
- Filter by level (DEBUG / INFO / WARN / ERROR)
- Search text and page through older records
- Pause or resume live refresh


### Analytics

View usage statistics and model invocation costs.

**Key Features:**

- Token consumption and daily cost trends
- Total cost and channel attribution
- Usage distribution by channel and skill
- Today, last 7 days and last 30 days


### Config

Validate and save editable non-secret settings. An ordinary save does not mutate an already constructed Runtime.

**Key Features:**

- Edit language, logging/tracing, memory/knowledge switches and budgets
- Inspect saved values and redacted running values under `_meta.running`
- Restore pending state on refresh or re-login from server-side `_meta.pending_paths` and `_meta.restart_required`

Model credentials and gateway authentication remain read-only here; use the server configuration file or supported setup commands. Nested PATCHes merge leaves and cannot bypass secret restrictions. Unknown fields return 400. Saving uses atomic replacement; an invalid existing file returns 409 rather than being overwritten.

Restart reloads the configuration file and environment overrides. Environment precedence may keep a running value different from the file. There is no configuration history/automatic rollback button: restore a backed-up file, validate it and restart. Channel lifecycle operations below are the supported live activation boundary.


### Channels

Configure messaging channel connection parameters.

**Key Features:**

- View saved enablement, actual connection state and unapplied changes
- Enable/disable, start/stop/restart configured channels; an explicit lifecycle operation loads that channel's saved configuration
- Inspect failed or uncertain deliveries; check the destination chat before resuming sends or recording delivery

Maintain channel credentials in the server configuration file; this page does not create accounts or edit credentials.


---

## Access Control

The Dashboard inherits the gateway's authentication; it has no authentication settings of its own. `gateway.host` defaults to `127.0.0.1`, so only the local machine is served.

Management APIs retain API/admin token scopes. Session/turn transcripts require admin scope, including the Dashboard WS `sessions` subscription. Read tokens receive memory invalidations without entry IDs or principal scopes for aggregate refreshes. Platform/user allowlists separately authorize incoming messages. Same-origin browsers and same-origin WS handshakes without `Sec-Fetch-Site` work; cross-origin and unexpected Host requests are rejected. Configure `allowed_hosts` for the reverse proxy and explicitly allow a development frontend's origin only when needed.

!!! warning "Before exposing it publicly"
    Configure authentication and TLS termination at a reverse proxy before changing `gateway.host` to a non-loopback address. See [gateway authentication](../integrations/gateway/authentication.md) and [security hardening](../operations/security-hardening.md).

## Monitoring and Alerts

Through the Overview and Analytics pages, you can:

- Monitor system health status in real time
- Monitor model cost budgets
- Investigate failures using session/turn state and logs; the page does not configure general resource alerts
