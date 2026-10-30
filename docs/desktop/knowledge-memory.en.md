# Documents, knowledge, and memory

**Scope: Desktop.** Documents are deliverables, knowledge sources are reference material, and memory preserves reusable preferences and facts. Manage them according to their purpose.

## Generate and export documents

Specify format, content, and location. For example:

> Read the meeting notes in this directory and generate a Word action list with owners, actions, and deadlines. Mark missing information as “To confirm” and save it in this workspace.

The built-in generator supports Word, PDF, Excel, and PowerPoint without Office, LibreOffice, or Python. Task-generated files default to `EchoAgent成果/` in the workspace, and existing files are protected. Use **Export** on a response to choose a format and save location.

| Output | Scope |
| --- | --- |
| Word / PDF | Headings, paragraphs, lists, and tables |
| Excel | Tables become worksheets; structured generation supports a limited formula set, formatting, and native charts |
| PowerPoint | Headings organize slides; structured generation can specify pages, tables, and charts |

Exact editing of existing layouts, OCR, and PDF rearrangement require dedicated workflows. Excel support is not a complete calculation engine. Unsupported functions, external workbooks, and macros are limited. Preview is not the original application's layout engine; open the final file for acceptance.

## Use knowledge sources

Select task materials or a local knowledge source and ask for an answer based on them. Indexing divides extractable text into chunks and retrieves relevant passages. Unextracted content or text beyond indexing budgets is not automatically available.

Request sources for conclusions and explicit acknowledgement of missing evidence. Check index freshness after updating material. Personal and organization knowledge follow selected sources. Source private chats are shared into an expert group only when explicitly selected.

## Manage long-term memory

Open **Settings → Memory** for the main switch, contents, and provenance. Post-task extraction can retain supported preferences, work background, project facts, and recent actions. Global preferences can span projects; project facts are scoped to workspaces.

Add, correct, forget, inspect sources, preview text or JSON imports, and copy exports. Automatic maintenance does not overwrite manually saved memories. Temporary items can be completed or expire.

Corrections and forgetting exclude outdated facts and related sources from later use. Disabled entries remain inspectable and original chats remain in history. Turning memory off stops subsequent recall and extraction while keeping existing content manageable.

## Choose the right storage

| Content | Store as |
| --- | --- |
| Deliverable reports and tables | Verified files |
| Project standards and references | Knowledge sources with provenance and updates |
| Stable preferences and project facts | Scoped memory |
| Current task progress | Session and execution records |

Retrieval may use remote embedding and reranking services. Local storage does not imply retrieval text stays offline. See [Data boundaries](operations.md).
