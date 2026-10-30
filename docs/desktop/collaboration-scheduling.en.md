# Expert groups and scheduled tasks

**Scope: Desktop.** Groups retain roles, discussion, work, and decisions. Scheduled tasks start local execution at configured times. Both use the embedded engine.

## Create a group

Create a group from the experts, skills, and connectors area. Select 1–8 local experts, a model, and a workspace; a coordinator is added automatically. A session team can also create a group; already linked teams open their existing group.

Ordinary messages go to the coordinator. Mentions or replies select recipients. Start with read-only discussion to clarify goals before execution. The engine restricts tool permissions for read-only discussion.

Member, work, deliverable, and settings views expose responsibilities, execution state, generated files, and configuration. Templates include only explicitly saved members, roles, and models, without conversation history.

## Collaborate and record decisions

The coordinator can delegate and summarize results. Members use independent sessions. Execution in the same workspace is serialized, with two concurrent members per group by default. Automatic-call, depth, and Token limits bound collaboration.

Model-proposed decisions require confirmation before becoming shared working context. Decisions can be corrected, invalidated, or restored with revision history. Member memory, shared decisions, and chat history are separate. Clearing member memory does not delete chats or decisions.

Reply to the relevant work message when refining a request. Pausing stops further dispatch while retaining queued preparation; stopping cancels active and pending-summary work. Restart-interrupted work requires inspection and manual retry.

Archiving is reversible. Permanent deletion cleans up group-owned records while retaining personal experts, saved templates, and workspace files.

## Create a scheduled task

Configure the goal, schedule, directory, model, and permissions. Run it manually first, inspect records and output, then enable the schedule.

The scheduler runs inside the desktop process. Shutdown, sleep, or application exit prevents execution at the scheduled time. Use a standalone [Runtime service](../operations/background-service.md) for persistent hosting. Desktop schedules and Runtime Cron are managed separately.

Records distinguish queued, running, successful, failed, and interrupted work. A due schedule does not prove task success. Avoid unattended tasks that depend on temporary dialogs or manual interaction. Check notification or WeChat connections separately when enabled.

## Verify collaborative delivery

Check usable member outputs, traceable references, coverage of the original goal, and unresolved failures. See [Multi-Agent](../concepts/multi-agent.md) for the design.
