---
name: workflow-chain
description: "Run reviewed JSON command stages sequentially or in parallel, with per-step timeout and explicit failure handling."
version: 1.0.0
metadata:
  echo:
    tags: [Workflow, Automation, Chain, Pipeline, Orchestration]
---

# Workflow Chain

Orchestrate multi-step workflows by chaining skills and actions.

## Execution boundary

The bundled `workflow_engine.py` reads JSON files with `stages` and command `steps`. It does not scan `~/.echo-agent/workflows/`, parse YAML skill recipes, substitute `{{variables}}`, evaluate `when`, retry failed steps, or register cron/keyword/event triggers. A command must name an actual executable and its arguments; this is separate from the core Agent-task DAG `workflow` tool.

To combine skills, the Agent reads their instructions and invokes supported tools explicitly. Use the core `cronjob` tool with human approval for periodic execution; a JSON workflow file alone schedules nothing.

## Security

**Only run trusted workflow files.** The engine runs the commands listed in the
JSON workflow definition. Never run workflow files from untrusted sources
(downloads, user uploads, model-generated content) without manual review.

Each `command` is parsed with `shlex` and executed as a single program with a
fixed argument list — **shell syntax is not interpreted**. `;`, `&&`, `|`, `>`
and `$(...)` are passed through as literal arguments, so one step cannot chain
into another command. A step that genuinely needs a pipeline or redirection has
to ask for a shell explicitly:

```json
{"command": "sh -c 'grep ERROR app.log | wc -l > count.txt'"}
```

Writing it that way keeps the intent visible in the workflow file instead of
every step carrying implicit shell power.

## Supported command stages

- `mode: "sequential"` runs steps in order; `"parallel"` uses a bounded thread pool.
- Each step has a `command`, optional `timeout` and `ignore_error`.
- `ignore_error` permits continuation after that step's failure; it is not rollback or automatic retry.
- Outputs are collected for reporting, not interpolated into later commands.

## Script

```bash
python3 scripts/workflow_engine.py run workflow.json          # run a workflow from JSON file
python3 scripts/workflow_engine.py inline "echo hello" "echo done"  # chain commands inline
python3 scripts/workflow_engine.py template --output my-workflow.json  # generate a template
```

Workflow JSON format:
```json
{
  "name": "my-workflow",
  "stages": [
    {"name": "build", "mode": "sequential", "steps": [{"command": "echo build"}]},
    {"name": "test", "mode": "parallel", "steps": [{"command": "echo t1"}, {"command": "echo t2"}]}
  ]
}
```
