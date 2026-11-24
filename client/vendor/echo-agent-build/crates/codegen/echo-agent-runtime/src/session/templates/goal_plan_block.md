A structured plan for this goal is on disk — the source of truth for "done". Read it first and keep it open.

Plan: {PLAN_PATH}

- Seed todos from the plan's acceptance criteria via {TODO_TOOL} before executing.
- If the plan has a `## Task checklist`, work it in order and flip each `- [ ]` to `- [x]` in the plan file as you complete it — the harness mines the first unchecked box as your next-step nudge, so a stale checklist produces stale nudges.
- Execute item by item; when you deviate, append a bullet to the plan's single `## Deviations` section — add to that one section; don't start a new one, and don't edit the plan's existing items. Keep it TERSE: ONE bullet per deviation (what changed + why); not a progress log, so don't restate the plan or dump test counts / "all fixed" / "verification re-run" / "superseding" notes there.
- Before claiming completion, run the plan's `## Verification plan` yourself and confirm its observations hold. SAVE durable proof: inspect the result against the acceptance criteria and write the captured observations to your scratch dir (the one the goal rules name; never shared `/tmp/...`). Fix any missing observation before calling the goal complete.
