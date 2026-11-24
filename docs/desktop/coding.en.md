# Code tasks and file changes

An EchoAgent conversation can read and change files in an authorized working folder. Code tasks use the same model selection, tool activity, and approvals as other conversations.

## Start a code task

1. Choose the folder containing your code, a model, and a permission mode on the home screen.
2. Describe the change, the behavior to preserve, and how to verify it.
3. Follow tool activity. When prompted for approval, check the operation and target folder.

For example, ask the agent to fix a known issue, add the relevant tests, and report the changed files and test results. Prepare project interpreters, compilers, and dependencies on the machine that hosts the working folder.

## Review changes and results

Open the file-changes panel in the conversation to review diffs, or inspect current files in the workspace file tree. Check the actual test output and any failures. If the work needs another pass, follow up in the same conversation.

[Tasks, sessions, and permissions](tasks-permissions.md) · [Data management and recovery](operations.md)
