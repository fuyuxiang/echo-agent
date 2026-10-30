# Tasks, sessions, and permissions

**Scope: Desktop.** A task defines a goal; a session preserves its conversation and execution records; a workspace identifies relevant files. Continue a goal in its existing session and create a new task for unrelated work.

## Describe a verifiable task

Specify inputs, scope, deliverables, and acceptance criteria. For example:

> Read the three meeting notes in this directory. Summarize confirmed decisions and unfinished actions into “meeting-summary.docx”, citing the source file for each action. Preserve the original notes.

Projects and work items organize ongoing work and shared background. Execution still happens in sessions. Confirm the new location is relevant before changing a working directory.

## Choose a permission mode

| UI option | Behavior | Use case |
| --- | --- | --- |
| Request approval | Ask for operations requiring authorization | First use or unfamiliar projects |
| Automatic assessment | Approve low-risk operations and ask when needed | Well-scoped daily work |
| Approve this task | Avoid repeated approval for ordinary tools | Fully trusted tasks |

The home-screen selection applies to the next task. Changes inside an existing session apply to that session. Desktop modes and Runtime approval configuration are managed separately.

Permission mode, workspace authorization, and automation authorization all contribute to the execution boundary. Approving a task does not grant OS permissions. Browser side effects, explicit computer-task authorization, and OS access have separate checks.

## Inspect execution

Plans describe intended work, tool activity records actual operations, and Diffs show changes. A planned step has not necessarily run; a successful tool may still need result verification.

Check parameters and paths before approving. After declining, request read-only analysis, a different location, or a proposed approach. For failures, inspect the cause before changing dependencies, input, or permissions.

## Refine, pause, and resume

Use the same session to preserve context. Computer Use accepts “pause” or `/pause`, and “continue saving” or `/resume`; a resume button is also available. Automation tasks remain paused after restart. Loading history does not resume execution.

Inspect existing results before resuming, especially saves, sends, or submissions. History supports rewind and branching, but a conversation branch does not undo changes already made to files or external systems.

## Finish a task

Check deliverables, Diffs, and verification. Record unfinished work explicitly and save important outputs as actual files. See [Data management](operations.md) for backups.
