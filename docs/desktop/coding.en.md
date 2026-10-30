# Coding workspace

**Scope: Desktop Echo Code.** The workspace combines Agent conversations with an embedded Theia IDE offering a file tree, editor, search, terminal, Git, previews, and change review.

## Start a coding task

1. Select and authorize the actual project directory.
2. Open the coding workspace and inspect files and Git status.
3. Describe the issue, allowed scope, and verification commands.
4. Inspect the plan, execution, Diff, and verification results.

Example:

> Fix duplicate form submission while preserving current interaction behavior. Inspect tests and uncommitted changes first. Modify only relevant logic, run appropriate checks, and describe what was verified.

Inspect pre-existing changes before starting. Review the actual Diff to establish the scope of generated changes.

## Understand the stages

The workspace organizes analysis, planning, implementation, verification, and delivery. Change sets summarize affected files, verification records show checks that ran, and delivery reports explain outcomes and remaining issues. Use stage information together with check results.

The IDE provides interactive editing. The embedded engine performs reasoning and tool execution. The host coordinates their workspace, changes, and state. IDE terminals run as the current OS user. Prepare project compilers, interpreters, dependencies, and services separately.

## Verify and deliver

| Check | Evidence |
| --- | --- |
| Scope matches the request | Actual Diff and affected files |
| Existing work is preserved | Initial Git status and uncommitted changes |
| The issue is fixed | Relevant tests, builds, or reproducible behavior |
| Delivery is ready | Consistent artifacts, verification, and open issues |

Checkpoints and change review help recovery and comparison; keep project version control and data backups as well.

## Connection recovery

The application attempts to verify and reconnect the local IDE backend. If edits are unsaved or their state is uncertain, preserve the page and copy important content before reconnecting manually. Changing projects restarts the relevant backend.

See [Troubleshooting](operations.md) and [Desktop architecture](../concepts/desktop-architecture.md).
