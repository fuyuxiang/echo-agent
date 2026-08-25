---
name: workflow-chain
description: "Chain multiple skills and actions into named workflows with error handling and conditional branching."
version: 1.0.0
metadata:
  echo:
    tags: [Workflow, Automation, Chain, Pipeline, Orchestration]
---

# Workflow Chain

Orchestrate multi-step workflows by chaining skills and actions.

## Workflow Definition

Workflows are YAML files in `~/.echo-agent/workflows/`:

```yaml
name: morning-routine
trigger: cron "0 8 * * *"
steps:
  - id: get_weather
    skill: weather
    input: {location: "Beijing"}

  - id: get_todos
    skill: reminder
    action: due

  - id: compose
    skill: daily-briefing
    input:
      weather: "{{get_weather.output}}"
      todos: "{{get_todos.output}}"

  - id: deliver
    action: send
    channel: telegram
    content: "{{compose.output}}"
```

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

## Features

- **Variable interpolation**: `{{step_id.output}}` references previous step results
- **Conditional steps**: `when: "{{get_weather.temp}} < 5"`
- **Error handling**: `on_error: skip | abort | retry(3)`
- **Parallel steps**: `parallel: [step_a, step_b]`

## Built-in Templates

### Research Pipeline
```yaml
name: research-pipeline
steps:
  - {id: search, skill: web-search, input: {query: "{{topic}}"}}
  - {id: extract, skill: web-extract, input: {urls: "{{search.top_urls}}"}}
  - {id: report, skill: deep-research, input: {content: "{{extract.output}}"}}
```

### Morning Routine
```yaml
name: morning-routine
trigger: cron "0 8 * * *"
steps:
  - {id: weather, skill: weather, input: {location: "Beijing"}}
  - {id: briefing, skill: daily-briefing}
  - {id: send, action: send, channel: auto, content: "{{briefing.output}}"}
```

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

## Triggers

| Type | Example |
|------|---------|
| Cron | `cron "0 8 * * *"` |
| Keyword | `keyword "生成报告"` |
| Manual | `manual` |
| Event | `event "new_message"` |
