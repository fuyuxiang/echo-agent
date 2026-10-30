# Security model

Permissions determine which operations a request may perform. Isolation determines what those operations can affect. Echo Agent combines intake authentication, tool policy, operation checks, and approvals; deployment, execution backends, and OS access define the actual boundary.

## Layers

| Layer | Checks | Purpose |
| --- | --- | --- |
| Intake | Senders, tokens, origins, pairing | Admit authorized requests |
| Scope | Sessions, ownership, memory, workspaces | Select accessible data |
| Tool admission | Profiles, capabilities, explicit denials | Narrow requested operations |
| Operation constraints | Paths, commands, destinations, parameters | Constrain targets |
| Approval | Allow, ask, deny | Obtain additional authorization |
| Environment | Local, container, remote backend | Determine process and filesystem boundaries |

Checks are distributed across call paths; not every tool follows an identical linear pipeline.

## Runtime policies

Security profiles distinguish personal, daemon, and public-Gateway use. Tool profiles distinguish minimal, messaging, coding, and full sets. Effective capability follows the combined policies.

Calls can be allowed, held for approval, or denied. Unattended requests cannot rely on interactive approval to acquire high-risk permissions. Smart approval assesses risk without overriding hard denials.

Command checks reject recognized dangerous forms; file tools validate paths; protected fetches check protocols, addresses, DNS, and redirects. This does not establish equivalent isolation for all third-party code or external tools.

## Multiple clients and tenants { #multi-client-tenant-boundary }

One instance supports multiple clients and sessions. Session IDs primarily route context. Ordinary and admin tokens establish authentication and administrative levels, not complete tenant partitions.

Use separate instances, processes, data directories, databases, workspaces, credentials, and network entry points for mutually untrusted users or organizations. Fine-grained ownership on one interface does not establish the same boundary everywhere.

## Desktop authorization

Desktop combines authorized directories, task permission modes, and capability authorization. Browser side effects receive separate confirmation. Computer tasks require explicit requests, run continuously within the task, and lose old authorization on pause or stop. OS permissions are controlled by the system.

A desktop workspace is not an OS filesystem sandbox. IDE terminals run as the current user. Read-only discussion, ordinary tool approval, and computer-task authorization are distinct mechanisms.

## Data and credentials

Runtime-managed credentials can be encrypted, but model keys and channel tokens entered directly in configuration are not automatically moved into that store. Preserve matching keys during migration.

Desktop model configuration can also contain API keys. Protect local files. Local storage does not imply that models, MCP, connectors, or retrieval services receive no task data.

## Content versus instructions

Websites, attachments, tool outputs, and extracted memory are reference material, not independent sources of user authorization. Memory scans, provenance, and skill admission reduce untrusted influence without guaranteeing detection of every injection.

See [Tools and permissions](../guides/tools-permissions.md), [Hardening](../operations/security-hardening.md), and [Desktop permissions](../desktop/tasks-permissions.md).
