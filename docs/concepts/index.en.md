# Core concepts

This section explains how the system works and which problems each mechanism solves. Start with [Architecture](architecture.md), then choose a topic. Look up operational commands and parameters in guides and reference pages.

| Question | Topic | Scope |
| --- | --- | --- |
| What do Desktop and Runtime do? | [Architecture](architecture.md) | Both |
| How does the desktop host coordinate execution, IDE, and automation? | [Desktop architecture and data flow](desktop-architecture.md) | Desktop |
| How does a request become a result? | [Agent loop](agent-loop.md) | Shared method, Runtime details |
| How do multiple entry points reuse execution? | [Events and delivery](events-delivery.md) | Runtime |
| How are conversations, memory, and identity separated? | [Workspace, session, and identity](workspace-session-identity.md) | Both, separately described |
| How are durable facts retained and recalled? | [Memory](memory-system.md) | Runtime with Desktop comparison |
| How are long histories and tool outputs handled? | [Compression and output preservation](context-compression-spill.md) | Runtime |
| Which extension mechanism fits? | [Tools, skills, and plugins](skills-tools-plugins.md) | Both, separate formats |
| How do permissions differ from isolation? | [Security](security-model.md) | Both |
| How does experience become a skill? | [Evolution and evaluation](evolution-evaluation.md) | Runtime |
| How do Agents divide and summarize work? | [Multi-Agent](multi-agent.md) | Both, separate mechanisms |

## Diagram conventions

Architecture uses C4-style views with explicit levels, boundaries, and relations. Interactions use UML sequence diagrams; lifecycles use UML state diagrams. Flowcharts use terminals, processing rectangles, and decision diamonds with labeled branches. Data-flow diagrams use Yourdon / DeMarco notation: external rectangles, process circles, parallel-line data stores, and named directional flows.

Data-flow diagrams do not show execution order. Dependencies do not imply shared state. Optional conditions affecting usage are stated explicitly. See [Documentation contribution](../development/documentation.md) for the complete conventions.

Click SVG figures to view them at full size.
