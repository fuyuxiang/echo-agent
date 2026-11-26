<system-reminder>
<goal-state>
Objective: {objective}
Status: Active
Tokens: {tokens} | Elapsed: {elapsed}
</goal-state>

{bail_preface}{plan_pointer}{verifier_gaps}{strategist_note}{reverify_block}Goal NOT complete — continue working. Next step:
{next_step}

Keep your {todo_tool} list current (≥1 `in_progress`, descriptive `activeForm`). Run targeted tests after every change you make, not just at the end. Tests must drive the SHIPPED code on the real path — no hard-coded values, no starting past the thing under test, no re-implementing it. Use your scratch dir {scratch_dir} {scratch_status} only for captured test output, temp scripts, and throwaway artifacts, never shared `/tmp/...`. Use existing user, system, or project defaults for execution dependencies and environment state. NEVER set `HOME`, `CARGO_HOME`, `RUSTUP_HOME`, package-manager homes, virtualenvs, caches, or config dirs to scratch, or persist references to scratch, which is deleted when the goal ends. The plan's `{SCRATCH}` placeholder resolves there. The verifier AUDITS your committed tests and saved evidence rather than rebuilding them — leave honest proof or you WILL be refuted. Before calling `{goal_tool}(completed: true)`, run the plan's `## Verification plan` steps yourself and confirm the observations it lists hold — the harness re-checks against those SAME steps each attempt and inlines any outstanding verifier gaps above.
</system-reminder>
