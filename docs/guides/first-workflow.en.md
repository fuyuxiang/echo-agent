# Your first Runtime workflow

**Scope: standalone Runtime.** Start with read-only work, then introduce files and durable information. Complete the [quickstart](../getting-started/quickstart.md) first.

## 1. Prepare controlled input

Create a Markdown or text file with verifiable facts in a directory Runtime can read. Avoid sensitive business material for initial testing.

> Read the specified file and list three facts supported by exact source passages. Say when information is absent. Do not modify files yet.

Supply a real path within the active workspace and file policy. If reading is unavailable, inspect enabled tools and the tool profile. If denied, fix the scope rather than asking the model to bypass restrictions.

## 2. Verify evidence

Check that the actual file was read, each fact is supported, and general knowledge was not presented as source content. This verifies file tooling, feedback, and context use together.

## 3. Generate a file

Ask for the verified content as Markdown at an allowed output location. Writing needs an enabled tool and permission. Check the path during approval, then read the actual saved file.

Use artifact delivery for downloadable reports or attachments. Temporary spilled-output references are not finalized deliverables.

## 4. Separate history, memory, and knowledge

| Need | Mechanism | Verification |
| --- | --- | --- |
| Continue discussing a file | Same session | Follow-up matches source and previous context |
| Retain a stable preference | Scoped memory | Inspect saved entry and provenance |
| Repeatedly search documents | Indexed knowledge | Check index state, passages, and sources |

Consolidation has trigger conditions; it does not persist every sentence immediately. Refresh and verify document indexes instead of relying on a request to “remember the new version”.

## 5. Introduce automation

Configure channels or schedules after a manual task works reliably. Specify input, directory, model, tool authorization, output, and failure handling. Trigger once manually and inspect the result before scheduling.

Read [Routing](models/routing-fallback.md), [Permissions](tools-permissions.md), [Knowledge](knowledge-base.md), and [Scheduled jobs](scheduled-jobs.md).
