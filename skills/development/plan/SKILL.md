---
name: plan
description: Plan mode — inspect context, write a markdown plan into the active workspace's `.echo-agent/plans/` directory, and do not execute the work.
version: 1.0.0
metadata:
  echo:
    tags: [planning, plan-mode, implementation, workflow]
---

# Plan Mode

Use this skill when the user wants a plan instead of execution.

## Core behavior

For this turn, you are planning only.

- Do not implement code.
- Do not edit project files except the plan markdown file.
- Do not run mutating terminal commands, commit, push, or perform external actions.
- You may inspect the repo or other context with read-only commands/tools when needed.
- Your deliverable is a markdown plan saved inside the active workspace under `.echo-agent/plans/`.

## Output requirements

Write a markdown plan that is concrete and actionable.

Include, when relevant:
- Goal
- Current context / assumptions
- Proposed approach
- Step-by-step plan
- Files likely to change
- Tests / validation
- Risks, tradeoffs, and open questions

If the task is code-related, include exact file paths, likely test targets, and verification steps.

## Save location

Save the plan with `write_file` under:
- `.echo-agent/plans/YYYY-MM-DD_HHMMSS-<slug>.md`

Use this path relative to the active working directory.

If the runtime provides a specific target path, use that exact path.
If not, create a sensible timestamped filename yourself under `.echo-agent/plans/`.

## Interaction style

- If the request is clear enough, write the plan directly.
- If no explicit instruction accompanies `/plan`, infer the task from the current conversation context.
- If it is genuinely underspecified, ask a brief clarifying question instead of guessing.
- After saving the plan, reply briefly with what you planned and the saved path.
