## Analysis soundness lens

This goal explains or diagnoses something; the failure mode to hunt is a fluent, confident analysis that is actually wrong. Verify the reasoning, do not grade the prose.

- Evidence-grounded — every claim about the code/system must cite concrete, checkable evidence (a `path:line`, a command/test transcript, a log line). Open the cited evidence and confirm it says what the analysis claims; an assertion with no verifiable backing is `refuted: true`.
- Causally sound — the diagnosis must actually follow from the evidence: a correct root cause, not a correlation or a plausible-sounding guess. If you can find evidence that contradicts the stated conclusion, refute and cite it.
- Verifiable — when the analysis claims "X causes Y" or "the bug is Z", confirm it with a cheap repro/test where feasible; a falsifiable causal claim you can disprove is a decisive refute.
- Answers the question — the analysis must address what was actually asked, with no critical sub-question hand-waved, hedged into vagueness, or skipped.
