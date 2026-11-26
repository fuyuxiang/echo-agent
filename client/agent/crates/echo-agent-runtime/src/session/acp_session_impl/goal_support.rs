//! Goal-harness support for `SessionActor`: reminder/directive templates,
//! goal wrappers, availability, and goal token accounting.

use super::*;

/// Number of consecutive non-completing goal-mode turns before the goal
/// auto-pauses with `GoalPauseReason::BackOff`. See `handle_turn_end`.
/// Compile-time constant for v1; remote tunability is a deferred follow-up.
pub(super) const GOAL_CONTINUATION_BACKOFF_THRESHOLD: u32 = 3;

/// Upper bound on planner attempts per `maybe_run_goal_planner` call, so
/// repeated steering / interruptions cannot spin the retry loop forever.
pub(super) const GOAL_PLANNER_MAX_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy)]
pub(crate) struct GoalClassifierPolicy {
    pub enabled: bool,
    pub max_runs: u32,
}

struct GoalPlannerStateGuard<'a> {
    tracker: &'a parking_lot::Mutex<crate::session::goal_tracker::GoalTracker>,
}

impl Drop for GoalPlannerStateGuard<'_> {
    fn drop(&mut self) {
        if let Some(state) = self.tracker.lock().take_planner_run() {
            state.cancel.cancel();
        }
    }
}

/// What `run_goal_planner_attempt` tells the `maybe_run_goal_planner` loop to
/// do next.
enum PlannerAttemptStep {
    /// Nothing to run or keep (disabled, no coordinator, goal changed/gone, plan
    /// present, or staging failed) — break.
    Stop,
    /// Interrupted with new steering — fold it in and replan.
    Steered(Vec<String>),
    /// Ran to a terminal outcome — the loop decides publish/pause.
    Ran {
        goal_id: String,
        plan_file: std::path::PathBuf,
        attempt_file: tempfile::TempPath,
        outcome: crate::session::goal_planner::GoalPlannerOutcome,
    },
}

impl DrainSource {
    /// Consume the source, returning `(input, Option<ack_tx>)`.
    /// `None` means the ack was already resolved.
    pub(super) fn into_parts(
        self,
    ) -> (
        echo_agent_tools::implementations::echo_agent_build::update_goal::UpdateGoalInput,
        Option<
            tokio::sync::oneshot::Sender<
                echo_agent_tools::implementations::echo_agent_build::update_goal::UpdateGoalAck,
            >,
        >,
    ) {
        match self {
            Self::Pending(i) => (i, None),
            Self::Channel((i, ack)) => (i, Some(ack)),
        }
    }
}

/// Send an `UpdateGoalAck` only if the ack channel is live (channel
/// source). No-op for `Pending` source — its ack was already resolved.
pub(super) fn try_send_ack(
    ack_tx: Option<
        tokio::sync::oneshot::Sender<
            echo_agent_tools::implementations::echo_agent_build::update_goal::UpdateGoalAck,
        >,
    >,
    ack: echo_agent_tools::implementations::echo_agent_build::update_goal::UpdateGoalAck,
) {
    if let Some(tx) = ack_tx {
        send_ack(tx, ack);
    }
}

pub(super) fn send_ack(
    ack_tx: tokio::sync::oneshot::Sender<
        echo_agent_tools::implementations::echo_agent_build::update_goal::UpdateGoalAck,
    >,
    ack: echo_agent_tools::implementations::echo_agent_build::update_goal::UpdateGoalAck,
) {
    if ack_tx.send(ack).is_err() {
        tracing::debug!("update_goal ack receiver dropped before harness could respond");
    }
}

pub(super) const GOAL_CLASSIFIER_PENDING_QUEUE_CAP: usize = 4;

pub(super) struct InFlightGuard<'a> {
    pub(super) flag: &'a std::sync::atomic::AtomicBool,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl NotAchievedSyntheticReason {
    /// Per-variant Markdown body for the synthetic details file. The
    /// exhaustive match forces a new variant to supply its own prose.
    pub(super) fn details_body(self, attempt: u32, max_runs: u32) -> String {
        match self {
            Self::ConcurrentInFlight => format!(
                "# Goal classifier — Not Achieved (synthetic)\n\n\
                 **Attempt:** {attempt} of {max_runs}\n\
                 **Reason:** ConcurrentInFlight\n\n\
                 The harness short-circuited this `update_goal(completed: true)` call \
                 because a classifier was already in flight for the previous completion \
                 attempt. The second attempt was accounted as Not Achieved (counting \
                 toward `classifier_max_runs`) to bound model retry spam.\n\n\
                 No sampler call was made; no token cost was incurred for this attempt.\n",
            ),
        }
    }

    /// One-line gaps gist inlined into the rejection nudge for the
    /// synthetic (no-sampler) `NotAchieved` path. Mirrors the bullet
    /// shape of `goal_classifier::build_gaps_summary` so the nudge
    /// reads uniformly regardless of which path produced it.
    pub(super) fn gaps_summary(self) -> String {
        match self {
            Self::ConcurrentInFlight => "- a verification was already running for the previous \
                 completion attempt; this duplicate `completed: true` was rejected without \
                 re-running the panel"
                .to_string(),
        }
    }
}

/// Disarm-able tracker scope-guard: runs `on_drop` on scope exit AND when a
/// state can never outlive its run.
///
/// No `GoalUpdated` from `Drop`: the Ctrl+C cancel path emits its own
/// auto-pause update strictly after the turn future is dropped, so cleared
/// state reaches the pager on that emit. `Drop` re-locks the non-reentrant
/// tracker mutex — never let the guard drop while holding that lock.
pub(super) struct TrackerDropGuard<'a, F: FnOnce(&mut crate::session::goal_tracker::GoalTracker)> {
    tracker: &'a parking_lot::Mutex<crate::session::goal_tracker::GoalTracker>,
    on_drop: Option<F>,
}

impl<'a, F: FnOnce(&mut crate::session::goal_tracker::GoalTracker)> TrackerDropGuard<'a, F> {
    pub(super) fn new(
        tracker: &'a parking_lot::Mutex<crate::session::goal_tracker::GoalTracker>,
        on_drop: F,
    ) -> Self {
        Self {
            tracker,
            on_drop: Some(on_drop),
        }
    }

    /// Cancel the drop action — the guarded state was applied normally.
    pub(super) fn disarm(&mut self) {
        self.on_drop = None;
    }
}

impl<F: FnOnce(&mut crate::session::goal_tracker::GoalTracker)> Drop for TrackerDropGuard<'_, F> {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f(&mut self.tracker.lock());
        }
    }
}

/// Result of [`SessionActor::resume_goal()`] (`/goal resume`).
pub(super) enum GoalResumeOutcome {
    /// Resume/nudge succeeded: run an inference turn seeded with `reminder`
    /// as the turn content; `user_msg` is the slash-command output.
    Inference { reminder: String, user_msg: String },
    /// Terminal/no-op (`AlreadyComplete` | `BudgetLimited` | `NoGoal` |
    /// missing goal state): print this message and end the turn.
    Message(String),
}

/// Goal-only `<task_completion_discipline>` (Rules 1–4); `{TODO_TOOL}` from [`GoalToolNames`].
/// Template must end with `\n` so `{DISCIPLINE_BLOCK}TRACKING:` glues correctly.
pub(super) fn render_goal_task_discipline(names: &GoalToolNames) -> String {
    GOAL_TASK_DISCIPLINE_TEMPLATE.replace("{TODO_TOOL}", &names.todo)
}

/// Render the plan-aware reminder block. `Plan: <abs path>` renders
/// on its own column-0 line — a single line-delimited pointer the
/// model and any downstream consumer (debug log scraper, support
/// tooling) can extract reliably, so keep the format stable.
pub(super) fn render_goal_plan_block(plan_path: &std::path::Path, names: &GoalToolNames) -> String {
    debug_assert!(
        !plan_path.as_os_str().is_empty(),
        "render_goal_plan_block requires a non-empty plan_path; an \
         empty path renders a dangling `Plan:` line that the model \
         cannot follow",
    );
    // Column-0 single-line `Plan: <abs>` contract — see fn docs.
    GOAL_PLAN_BLOCK_TEMPLATE
        .replace("{PLAN_PATH}", &plan_path.display().to_string())
        .replace("{TODO_TOOL}", &names.todo)
}

/// Plan path for the goal-mode reminder, or `None` on the legacy path.
/// `Some` only when the planner is enabled (`ECHO_AGENT_GOAL_PLANNER`) and a plan
/// exists; disabled ⇒ `None` ⇒ legacy block (no dangling `Plan:` line).
/// All three render sites (`setup_goal`, `resume_goal`, continuation nudge)
/// route through this helper so the gate can't drift. Borrows, no alloc.
pub(super) fn goal_reminder_plan_path(
    planner_enabled: bool,
    orchestration: &crate::session::goal_tracker::GoalOrchestration,
) -> Option<&std::path::Path> {
    planner_enabled
        .then_some(orchestration.plan_file.as_deref())
        .flatten()
}

/// Worker rounds a refuted goal may run without re-firing verification
/// before the continuation directive escalates to a forceful "re-verify
/// now" block. A refuted weak model can otherwise churn indefinitely —
/// Override with `ECHO_AGENT_GOAL_REVERIFY_AFTER` (floored at 1).
pub(crate) const GOAL_REVERIFY_AFTER_DEFAULT: u32 = 8;

/// Stable substring present in every rendered continuation directive
/// (from [`GOAL_CONTINUATION_DIRECTIVE_TEMPLATE`]) and in no other
/// reminder. Used to find and drop the prior turn's directive from
/// history so only the latest copy persists.
pub(super) const GOAL_CONTINUATION_SENTINEL: &str =
    "Goal NOT complete — continue working. Next step:";

/// Bail-specific preface substituted into the `{bail_preface}` slot of
/// [`GOAL_CONTINUATION_DIRECTIVE_TEMPLATE`] when the turn-final text
/// matched a [`goal_stop_detector`](super::goal_stop_detector) pattern
/// while pending todos remained. Names the apparent stop and the
/// outstanding work, then lets the unchanged generic body carry the
/// next-step / token / Rule-1/Rule-4 content. The generic flavor
/// substitutes the empty string for this slot.
pub(super) const GOAL_CONTINUATION_BAIL_PREFACE: &str = "You appear to be stopping or handing off, but the goal is NOT complete \
     and todos remain. Do not end the turn here — keep working.\n\n";

/// Render the shared goal-rules template with tool names and
/// site-specific blocks substituted in.
///
/// The template is the slim current form: verification is owned by
/// the harness (the adversarial skeptic panel in `goal_classifier.rs`),
/// so this body only carries TRACKING / WORKING / VERIFY / TEST
/// verdict-file path is substituted — `{VERIFIER_ID}` no longer
/// appears in the template; `verifier_id` continues to anchor
/// harness-owned skeptic verdict files inside `goal_classifier.rs`.
///
/// `block_recap` and `goal_state` are inserted verbatim — pass an
/// empty string to omit either section. The trailing newline of the
/// template is preserved so callers can append their closing
/// directive ("Start now." / "Continue working now.") and the
/// closing `</system-reminder>` tag without extra glue.
///
/// `plan_path` `Some` folds the plan-aware preamble into the same block as
/// the discipline; `None` renders the no-plan block byte-for-byte unchanged.
///
/// Legacy artifacts (the deleted COMPLETION AUDIT / canonical
/// verifier blocks / `{VERIFIER_ID}` placeholder) are pinned absent
/// by `goal_rules_template_drops_all_legacy_verifier_artifacts`.
pub(super) fn render_goal_rules(
    objective: &str,
    names: &GoalToolNames,
    block_recap: &str,
    goal_state: &str,
    plan_path: Option<&std::path::Path>,
    scratch_dir: &str,
    scratch_ready: bool,
) -> String {
    let discipline = render_goal_task_discipline(names);
    let plan_block = match plan_path {
        Some(path) => render_goal_plan_block(path, names),
        None => String::new(),
    };
    GOAL_RULES_TEMPLATE
        .replace("{OBJECTIVE}", objective)
        .replace("{TASK_TOOL}", &names.task)
        .replace("{TODO_TOOL}", &names.todo)
        .replace("{PLAN_BLOCK}", &plan_block)
        .replace("{BLOCK_RECAP}", block_recap)
        .replace("{DISCIPLINE_BLOCK}", &discipline)
        .replace("{GOAL_STATE}", goal_state)
        // literal `{SCRATCH_DIR}` in the objective WOULD be expanded here
        // (harmless, astronomically unlikely). The `{SCRATCH}` placeholder the
        // text references is a different token, left unreplaced.
        .replace("{SCRATCH_DIR}", scratch_dir)
        // Only claim the dir exists when the harness actually created it.
        .replace(
            "{SCRATCH_STATUS}",
            if scratch_ready {
                "The dir has been created for you."
            } else {
                "Create it with `mkdir -p` if it does not already exist."
            },
        )
}

pub(super) fn render_goal_rules_legacy(
    objective: &str,
    names: &GoalToolNames,
    block_recap: &str,
    goal_state: &str,
    plan_path: Option<&std::path::Path>,
    scratch_dir: &str,
    scratch_ready: bool,
) -> String {
    let discipline = render_goal_task_discipline(names);
    let plan_block = match plan_path {
        Some(path) => render_goal_plan_block(path, names),
        None => String::new(),
    };
    GOAL_RULES_TEMPLATE_LEGACY
        .replace("{OBJECTIVE}", objective)
        .replace("{GOAL_TOOL}", &names.goal)
        .replace("{TASK_TOOL}", &names.task)
        .replace("{TODO_TOOL}", &names.todo)
        .replace("{PLAN_BLOCK}", &plan_block)
        .replace("{BLOCK_RECAP}", block_recap)
        .replace("{DISCIPLINE_BLOCK}", &discipline)
        .replace("{GOAL_STATE}", goal_state)
        .replace("{SCRATCH_DIR}", scratch_dir)
        .replace(
            "{SCRATCH_STATUS}",
            if scratch_ready {
                "The dir has been created for you."
            } else {
                "Create it with `mkdir -p` if it does not already exist."
            },
        )
}

/// Assemble a goal auto-pause message: a `headline` summarizing why the
/// goal paused, the grouped-by-classification blocker `pause_summary`
/// (already sanitized upstream), and the `details_path` pointer. The
/// summary block is dropped when empty so a degenerate pause stays
/// well-formed.
pub(super) fn format_goal_pause_message(
    headline: &str,
    pause_summary: &str,
    details_path: &str,
) -> String {
    // An empty `details_path` means the harness has no artifact to point at
    // (e.g. the synthetic-details write was skipped on a squatted scratch
    // root); omit the "See …" pointer rather than dangle a bare "See".
    let has_path = !details_path.trim().is_empty();
    match (pause_summary.trim().is_empty(), has_path) {
        (true, true) => format!("{headline} See {details_path}"),
        (true, false) => headline.to_string(),
        (false, true) => format!("{headline}\n{pause_summary}\nSee {details_path}"),
        (false, false) => format!("{headline}\n{pause_summary}"),
    }
}

/// Make `{` / `}` inert in a model-controlled directive-slot value: a
/// later `.replace` pass and spoof a harness slot. A zero-width space
/// inside each brace keeps legitimate braces (code spans, JSON)
/// visually intact while no `{placeholder}` token can match; borrowed
/// pass-through when brace-free, so clean slots cost no allocation.
fn neutralize_directive_braces(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['{', '}']) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '{' => out.push_str("{\u{200b}"),
            '}' => out.push_str("\u{200b}}"),
            _ => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Full model-slot sanitization: brace neutralization
/// (anti-`{placeholder}` spoofing) plus reminder-tag neutralization
/// (anti-`</system-reminder>` frame escape). Applied uniformly at the
/// renderer so no slot's defense depends on its producer remembering an
/// axis; re-application to producer-neutralized values is a no-op, and
/// the tag pass only allocates when a frame-tag fragment is present.
fn neutralize_directive_slot(text: &str) -> std::borrow::Cow<'_, str> {
    use crate::session::goal_classifier::neutralize_reminder_tags;

    let out = neutralize_directive_braces(text);
    // Every tag the neutralizer breaks ends in one of these fragments.
    if out.contains("system-reminder>") || out.contains("goal-state>") {
        std::borrow::Cow::Owned(neutralize_reminder_tags(out.into_owned()))
    } else {
        out
    }
}

/// Render the per-turn directive continuation nudge.
///
/// Uses chained `.replace` calls rather than `format!` because the
/// template carries literal `{...}` examples inside backticks that
/// `format!` would force-escape. Placeholders are lowercase (see the
/// doc on [`GOAL_CONTINUATION_DIRECTIVE_TEMPLATE`]).
///
/// `objective` is load-bearing and guarded by `debug_assert!`: an
/// empty value renders an `Objective:` line the agent cannot resolve
/// to a goal. `next_step` is NOT guarded because the production
/// caller's `unwrap_or_else` already substitutes a non-empty fallback
/// (`Check your \`{todo_tool}\` list for next steps.`); guarding here
/// would only catch a future refactor that drops that fallback.
///
/// `plan_pointer` is inlined verbatim; `bail_preface`, `verifier_gaps`
/// and `next_step` pass through [`neutralize_directive_slot`] first but
/// are otherwise inlined as passed — pass [`GOAL_CONTINUATION_BAIL_PREFACE`] for
/// `bail_preface` when the stop-detector fired (and the empty string
/// otherwise); pass the empty string for `plan_pointer` when no plan is
/// available; pass [`render_verifier_gaps_block`]'s output for
/// `verifier_gaps` (empty string when the latest verdict carries no
/// gaps) so the freshest verifier findings render above the next-step
/// line; pass the caller's chosen fallback for `next_step` when the
/// plan yields no concrete step.
///
/// Model-controlled slots (`bail_preface`, `verifier_gaps`,
/// `strategist_note`, `next_step`) are sanitized via
/// [`neutralize_directive_slot`] before substitution — inert under any
/// `.replace` order and unable to close the surrounding
/// `<system-reminder>` frame. `objective` is user-authored and stays
/// verbatim. Pinned by
/// `render_goal_continuation_directive_order_dependent_substitution_pinned`.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_goal_continuation_directive(
    objective: &str,
    tokens: u64,
    elapsed: &str,
    bail_preface: &str,
    plan_pointer: &str,
    verifier_gaps: &str,
    strategist_note: &str,
    reverify_block: &str,
    next_step: &str,
    todo_tool: &str,
    scratch_dir: &str,
    scratch_ready: bool,
) -> String {
    debug_assert!(
        !objective.is_empty(),
        "render_goal_continuation_directive requires a non-empty objective; \
         an empty value leaves the Objective: line dangling in the nudge",
    );
    // Every model-controlled slot is sanitized (bail_preface is a
    // harness const today but sits in the same slot class).
    let bail_preface = neutralize_directive_slot(bail_preface);
    let verifier_gaps = neutralize_directive_slot(verifier_gaps);
    let strategist_note = neutralize_directive_slot(strategist_note);
    let next_step = neutralize_directive_slot(next_step);
    GOAL_CONTINUATION_DIRECTIVE_TEMPLATE
        .replace("{objective}", objective)
        .replace("{tokens}", &tokens.to_string())
        .replace("{elapsed}", elapsed)
        .replace("{bail_preface}", &bail_preface)
        .replace("{plan_pointer}", plan_pointer)
        .replace("{verifier_gaps}", &verifier_gaps)
        .replace("{reverify_block}", reverify_block)
        .replace("{next_step}", &next_step)
        .replace("{todo_tool}", todo_tool)
        // `{SCRATCH}` is left literal — the model resolves it to this dir.
        .replace("{scratch_dir}", scratch_dir)
        // Only claim the dir exists when the harness actually created it.
        .replace(
            "{scratch_status}",
            if scratch_ready {
                "(created for you)"
            } else {
                "(create with `mkdir -p` if missing)"
            },
        )
        .replace("{strategist_note}", &strategist_note)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_goal_continuation_directive_legacy(
    objective: &str,
    tokens: u64,
    elapsed: &str,
    bail_preface: &str,
    plan_pointer: &str,
    verifier_gaps: &str,
    strategist_note: &str,
    reverify_block: &str,
    next_step: &str,
    todo_tool: &str,
    goal_tool: &str,
    scratch_dir: &str,
    scratch_ready: bool,
) -> String {
    debug_assert!(
        !objective.is_empty(),
        "render_goal_continuation_directive_legacy requires a non-empty objective; \
         an empty value leaves the Objective: line dangling in the nudge",
    );
    let bail_preface = neutralize_directive_slot(bail_preface);
    let verifier_gaps = neutralize_directive_slot(verifier_gaps);
    let strategist_note = neutralize_directive_slot(strategist_note);
    let next_step = neutralize_directive_slot(next_step);
    GOAL_CONTINUATION_DIRECTIVE_TEMPLATE_LEGACY
        .replace("{objective}", objective)
        .replace("{tokens}", &tokens.to_string())
        .replace("{elapsed}", elapsed)
        .replace("{bail_preface}", &bail_preface)
        .replace("{plan_pointer}", plan_pointer)
        .replace("{verifier_gaps}", &verifier_gaps)
        .replace("{reverify_block}", reverify_block)
        .replace("{next_step}", &next_step)
        .replace("{todo_tool}", todo_tool)
        .replace("{goal_tool}", goal_tool)
        .replace("{scratch_dir}", scratch_dir)
        .replace(
            "{scratch_status}",
            if scratch_ready {
                "(created for you)"
            } else {
                "(create with `mkdir -p` if missing)"
            },
        )
        .replace("{strategist_note}", &strategist_note)
}

/// Render the `{reverify_block}` slot. Empty unless the goal has been
/// refuted at least once AND has run `>= threshold` rounds since the last
/// verification fired. Reframes the loop (passing verification is the ONLY
/// way to finish) and inlines the live count; the lead hardens past
/// `3 * threshold`.
pub(super) fn render_goal_reverify_block(
    rounds_since_verify: u32,
    refuted: bool,
    threshold: u32,
) -> String {
    if !refuted || rounds_since_verify < threshold {
        return String::new();
    }
    let lead = if rounds_since_verify >= threshold.saturating_mul(3) {
        "STOP DRIFTING — VERIFY THE REMAINING GAP NOW."
    } else {
        "Re-run the verification plan before continuing."
    };
    format!(
        "{lead} You have run {rounds_since_verify} rounds since the last \
         adversarial rejection. The host evaluates completion after every round. \
         Fix the single concrete gap rather than making cosmetic changes.\n\n"
    )
}

pub(super) fn render_goal_reverify_block_legacy(
    rounds_since_verify: u32,
    refuted: bool,
    threshold: u32,
    goal_tool: &str,
) -> String {
    if !refuted || rounds_since_verify < threshold {
        return String::new();
    }
    let lead = if rounds_since_verify >= threshold.saturating_mul(3) {
        "STOP DRIFTING — RE-VERIFY NOW."
    } else {
        "Re-verify before continuing."
    };
    format!(
        "{lead} You have run {rounds_since_verify} rounds since your last \
         verification without calling `{goal_tool}(completed: true)`. The ONLY \
         way to finish this goal is to PASS verification — not to keep editing. \
         If the plan's `## Verification plan` steps now hold, call \
         `{goal_tool}(completed: true)` THIS round to re-trigger the skeptic \
         panel. If they do NOT, name the SINGLE concrete gap still blocking it \
         and fix exactly that — do not make cosmetic changes to look busy.\n\n"
    )
}

/// Render the strategist-note block for the continuation directive.
/// `recommendation` is the capped, model-authored snippet read back from
/// the strategy note (persisted as `last_strategy_recommendation`); empty
/// ⇒ empty string, collapsing the `{strategist_note}` slot (same
/// empty-slot convention as `verifier_gaps`). Otherwise it renders a
/// narrative that tells the model to RE-READ its plan AND the strategy
/// note before continuing, then ends with a blank line so it stacks above
/// the "Goal NOT complete" sentinel. When no plan path is available
/// (planner disabled) the plan clause is dropped.
///
/// Injection hardening: the rendered note is wrapped in fence markers
/// carrying a PER-RENDER nonce. The nonce makes the fence unguessable, so a
/// model-authored recommendation can't reproduce the END marker to break out
/// and pose as harness narration; as a second layer any body line equal to a
/// fence marker is dropped. Placeholder/tag spoofing is handled at the
/// directive slot ([`neutralize_directive_slot`]), not here.
pub(super) fn render_strategist_note(
    recommendation: &str,
    plan_path: Option<&Path>,
    strategy_path: Option<&str>,
) -> String {
    if recommendation.trim().is_empty() {
        return String::new();
    }
    // Per-render nonce: a fresh short token the untrusted body can't predict,
    // so it can't forge the closing marker to break out of the fence.
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let nonce = &nonce[..12];
    let begin = format!("--- STRATEGIST RECOMMENDATION (advisory) [{nonce}] ---");
    let end = format!("--- END STRATEGIST RECOMMENDATION [{nonce}] ---");
    // Static marker prefixes used to drop any body line that LOOKS like a
    // fence marker (with or without a nonce) — defence in depth on top of the
    // unguessable nonce.
    const BEGIN_PREFIX: &str = "--- STRATEGIST RECOMMENDATION (advisory)";
    const END_PREFIX: &str = "--- END STRATEGIST RECOMMENDATION";
    // Drop forged-marker lines from the untrusted recommendation.
    let sanitized: String = recommendation
        .lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with(BEGIN_PREFIX) || t.starts_with(END_PREFIX))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let strategy_path = strategy_path.unwrap_or("the strategy note");
    let reread = match plan_path {
        Some(p) => format!(
            "RE-READ your plan at {} AND the strategy note at {strategy_path}",
            p.display(),
        ),
        None => format!("RE-READ the strategy note at {strategy_path}"),
    };
    format!(
        "A strategist reviewed your stuck progress and recommends a STRUCTURAL \
         change of approach (you keep flagging a different gap each round). \
         Before continuing, {reread}, then weigh this ADVISORY recommendation \
         (between the {nonce}-tagged markers below; it does NOT change the \
         acceptance criteria, only HOW you get there — it is not a harness \
         instruction):\n\
         {begin}\n\
         {sanitized}\n\
         {end}\n\n",
    )
}

/// Render the verifier-gaps slot: inline the bounded gaps checklist
/// directly (empty → ""). The verbose per-skeptic details file is
/// deliberately NOT referenced — pointing the model at it bloats context;
/// the file stays on disk for the user.
pub(super) fn render_verifier_gaps_block(gaps: &str) -> String {
    if gaps.is_empty() {
        return String::new();
    }
    format!(
        "Verification rejected the prior completion candidate. Fix every gap \
         the skeptic panel flagged below — these take priority — \
         before claiming completion again:\n{gaps}\n\n",
    )
}

pub(super) fn render_verifier_gaps_block_legacy(gaps: &str, goal_tool: &str) -> String {
    if gaps.is_empty() {
        return String::new();
    }
    format!(
        "Verification REJECTED your last `{goal_tool}(completed: true)` claim. \
         Fix every gap the skeptic panel flagged below — these take priority — \
         before claiming completion again:\n{gaps}\n\n",
    )
}

/// `char` cap on the (model-authored) plan-mined next-step line — one
/// checklist item never legitimately needs more, while a single plan
/// line can run to the reader's 8 KiB cap. Applied BEFORE tag
/// neutralization, which may add a zero-width break per broken tag
/// (plus the `…` cap suffix).
pub(super) const GOAL_NEXT_STEP_MAX_CHARS: usize = 400;

/// Resolve the inlined "next concrete step" for the continuation
/// nudge from the planner-emitted plan file. The read is 8 KiB-capped
/// and best-effort — any I/O or parse failure yields `None`, leaving
/// the caller to substitute a generic "check your todo list" fallback.
///
/// The plan item is model-authored: `char`-capped to
/// [`GOAL_NEXT_STEP_MAX_CHARS`], then reminder-frame tags are
/// zero-width-broken so it cannot close the `<system-reminder>` frame
/// it is inlined into.
///
/// Verifier gaps are NOT consulted here: a `NotAchieved` verdict's
/// findings are surfaced separately and prominently via
/// [`render_verifier_gaps_block`] (persisted in `last_classifier_gaps`),
/// so this slot carries only the plan's next item to avoid duplicating
/// the top gap.
pub(super) fn resolve_goal_next_step(plan_path: Option<&Path>) -> Option<String> {
    use crate::session::goal_classifier::{cap_chars, neutralize_reminder_tags};
    use crate::session::goal_next_step::first_unchecked_plan_item;

    plan_path
        .and_then(first_unchecked_plan_item)
        .map(|item| neutralize_reminder_tags(cap_chars(&item, GOAL_NEXT_STEP_MAX_CHARS)))
}

pub(super) fn format_blocked_chat_notification(reason: &str, detail: Option<&str>) -> String {
    let mut out = String::with_capacity(reason.len() + detail.map_or(0, str::len) + 96);
    out.push_str("Goal paused — verification blocked.\nReason: ");
    out.push_str(reason);
    out.push('\n');
    if let Some(detail) = detail {
        out.push_str(detail);
        out.push('\n');
    }
    out.push_str("\nType /goal resume to continue after you've addressed it.");
    out
}

/// Per-subagent token state; the goal-scoped marginal is
/// `last_cumulative_reported - resume_anchor_cumulative`.
///
/// Child reports carry the child's CONTEXT token total, so a child
/// compaction freezes the ratchet at its pre-compaction max until the
/// child's context regrows past it.
#[derive(Debug)]
pub(crate) struct SubagentTokenRecord {
    /// `None` for subagents spawned outside any active goal.
    pub goal_id: Option<String>,
    /// Parent's `last_cumulative_reported` at spawn; 0 for fresh spawns.
    pub resume_anchor_cumulative: u64,
    /// Monotonic high-water mark, ratcheted on `SubagentProgress` ticks
    /// and sealed by `SubagentFinished`.
    pub last_cumulative_reported: u64,
    /// Effective model id captured from `SubagentSpawned.model` at spawn
    /// time. Captured here (not at aggregation time) so attribution is
    /// pinned to the model the subagent actually ran on, even if the user
    /// switches the session model mid-goal. `None` (or empty) only when the
    /// wire field was absent; such records fold under the current model id
    /// as a best-effort fallback during aggregation.
    pub model: Option<String>,
    /// Set by `SubagentFinished`; later (stale or spoofed) progress ticks
    /// for the subagent are ignored so they can't move the ratchet.
    pub finished: bool,
}

impl SubagentTokenRecord {
    /// Goal-scoped marginal cost: `last_cumulative_reported -
    /// resume_anchor_cumulative`, saturating so a stale/out-of-order
    /// report below the anchor yields 0 instead of underflowing. Shared by
    /// the single-line total ([`SessionActor::goal_tokens`]) and the
    /// per-model breakdown ([`fold_tokens_by_model`]) so they can't drift.
    pub(crate) fn marginal(&self) -> u64 {
        self.last_cumulative_reported
            .saturating_sub(self.resume_anchor_cumulative)
    }
}

/// Fold subagent token records into a `model_id -> marginal_tokens`
/// breakdown for `goal_id`, sorted by tokens descending (ties broken by
/// model id for determinism). Marginal cost per record is
/// [`SubagentTokenRecord::marginal`]. Records whose `model` was absent or
/// empty/whitespace-only on the wire fold under `current_model_id`.
/// Zero-marginal records are skipped.
fn fold_tokens_by_model<'a>(
    records: impl IntoIterator<Item = &'a SubagentTokenRecord>,
    goal_id: &'a str,
    current_model_id: &'a str,
) -> Vec<(String, u64)> {
    let mut by_model: HashMap<&'a str, u64> = HashMap::new();
    for r in records {
        if r.goal_id.as_deref() != Some(goal_id) {
            continue;
        }
        let marginal = r.marginal();
        if marginal == 0 {
            continue;
        }
        // A missing OR empty/whitespace-only captured id folds under the
        // current model so we never create a blank-id bucket.
        let model = r
            .model
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(current_model_id);
        let entry = by_model.entry(model).or_insert(0);
        *entry = entry.saturating_add(marginal);
    }
    let mut out: Vec<(String, u64)> = by_model
        .into_iter()
        .map(|(m, t)| (m.to_owned(), t))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod fold_tokens_by_model_tests {
    use super::{SubagentTokenRecord, fold_tokens_by_model};

    fn rec(goal: Option<&str>, anchor: u64, last: u64, model: Option<&str>) -> SubagentTokenRecord {
        SubagentTokenRecord {
            goal_id: goal.map(str::to_owned),
            resume_anchor_cumulative: anchor,
            last_cumulative_reported: last,
            model: model.map(str::to_owned),
            finished: false,
        }
    }

    #[test]
    fn empty_records_yields_empty() {
        let records: Vec<SubagentTokenRecord> = Vec::new();
        assert!(fold_tokens_by_model(&records, "g1", "cur").is_empty());
    }

    #[test]
    fn mixed_models_sum_marginals_sorted_desc() {
        let records = vec![
            rec(Some("g1"), 0, 100, Some("echo-agent-3")),
            rec(Some("g1"), 100, 500, Some("echo-agent-4")), // marginal 400
            rec(Some("g1"), 0, 50, Some("echo-agent-3")),    // echo-agent-3 total 150
        ];
        let out = fold_tokens_by_model(&records, "g1", "cur");
        assert_eq!(
            out,
            vec![
                ("echo-agent-4".to_owned(), 400),
                ("echo-agent-3".to_owned(), 150)
            ]
        );
    }

    #[test]
    fn ties_break_by_model_id_ascending() {
        let records = vec![
            rec(Some("g1"), 0, 100, Some("zeta")),
            rec(Some("g1"), 0, 100, Some("alpha")),
        ];
        let out = fold_tokens_by_model(&records, "g1", "cur");
        assert_eq!(
            out,
            vec![("alpha".to_owned(), 100), ("zeta".to_owned(), 100)]
        );
    }

    #[test]
    fn none_model_folds_under_current() {
        let records = vec![rec(Some("g1"), 0, 100, None), rec(Some("g1"), 0, 200, None)];
        let out = fold_tokens_by_model(&records, "g1", "cur-model");
        assert_eq!(out, vec![("cur-model".to_owned(), 300)]);
    }

    #[test]
    fn single_distinct_model_collapses_to_one_entry() {
        let records = vec![
            rec(Some("g1"), 0, 100, Some("echo-agent-4")),
            rec(Some("g1"), 0, 200, None), // folds under current = echo-agent-4
        ];
        let out = fold_tokens_by_model(&records, "g1", "echo-agent-4");
        assert_eq!(out, vec![("echo-agent-4".to_owned(), 300)]);
    }

    #[test]
    fn other_goal_records_excluded() {
        let records = vec![
            rec(Some("g1"), 0, 100, Some("echo-agent-4")),
            rec(Some("g2"), 0, 999, Some("echo-agent-4")),
            rec(None, 0, 999, Some("echo-agent-4")),
        ];
        let out = fold_tokens_by_model(&records, "g1", "cur");
        assert_eq!(out, vec![("echo-agent-4".to_owned(), 100)]);
    }

    #[test]
    fn last_below_anchor_does_not_underflow() {
        let records = vec![rec(Some("g1"), 500, 100, Some("echo-agent-4"))];
        // marginal saturates to 0 -> skipped as a zero-token entry.
        assert!(fold_tokens_by_model(&records, "g1", "cur").is_empty());
    }

    #[test]
    fn captured_model_survives_mid_goal_current_model_switch() {
        // A record captured `echo-agent-4` at spawn keeps it even though the
        // current model at aggregation time is `echo-agent-3`.
        let records = vec![rec(Some("g1"), 0, 100, Some("echo-agent-4"))];
        let out = fold_tokens_by_model(&records, "g1", "echo-agent-3");
        assert_eq!(out, vec![("echo-agent-4".to_owned(), 100)]);
    }

    #[test]
    fn empty_or_whitespace_model_folds_under_current() {
        // `Some("")` and `Some("  ")` must NOT create a blank-id bucket;
        // they fold under the current model exactly like `None`.
        let records = vec![
            rec(Some("g1"), 0, 100, Some("")),
            rec(Some("g1"), 0, 200, Some("   ")),
            rec(Some("g1"), 0, 50, None),
        ];
        let out = fold_tokens_by_model(&records, "g1", "cur-model");
        assert_eq!(out, vec![("cur-model".to_owned(), 350)]);
    }

    #[test]
    fn empty_model_merges_with_current_model_bucket() {
        // An empty id folds into the SAME bucket as records that
        // explicitly captured the current model id.
        let records = vec![
            rec(Some("g1"), 0, 100, Some("echo-agent-4")),
            rec(Some("g1"), 0, 200, Some("")),
        ];
        let out = fold_tokens_by_model(&records, "g1", "echo-agent-4");
        assert_eq!(out, vec![("echo-agent-4".to_owned(), 300)]);
    }
}

/// Resolved per-role `/goal` model selection, cached on the actor.
///
/// `Default` (every role `InheritCurrent`, empty skeptic pool) reproduces
/// today's behavior — `runtime_overrides.model = None` + the role's default
/// `subagent_type`. The kill-switch (`goal_use_current_model_only`) collapses
/// all three to `InheritCurrent` at resolution time, so consumers never need
/// to re-check it.
#[derive(Debug, Clone, Default)]
pub(crate) struct GoalRoleModelConfig {
    /// Planner role choice (single pair or inherit).
    pub(crate) planner: crate::agent::config::GoalRoleModelChoice,
    /// Strategist role choice (single pair or inherit).
    pub(crate) strategist: crate::agent::config::GoalRoleModelChoice,
    /// Ordered skeptic pool; `pool[0]` is skeptic-0's model, the rest are
    /// assigned round-robin by index. Empty ⇒ all skeptics inherit.
    pub(crate) skeptic_pool: Vec<crate::util::config::GoalRoleModel>,
}

pub(crate) fn planner_failure_pause_message() -> String {
    "Planning failed; resume with /goal to retry.".to_string()
}

pub(crate) fn goal_slash_and_harness_available(goal_enabled: bool, tool_names: &[String]) -> bool {
    use echo_agent_tools::implementations::echo_agent_build::UPDATE_GOAL_TOOL_NAME;
    goal_enabled && tool_names.iter().any(|n| n == UPDATE_GOAL_TOOL_NAME)
}

/// Active `/goal` session with goal harness enabled (laziness, continuation, TodoGate goal arm).
pub(crate) fn laziness_injection_active(
    goal_harness_enabled: bool,
    goal_status: Option<crate::session::goal_tracker::GoalStatus>,
) -> bool {
    goal_harness_enabled && goal_status == Some(crate::session::goal_tracker::GoalStatus::Active)
}

impl SessionActor {
    pub(super) fn goal_harness_enabled(&self) -> bool {
        self.goal_harness_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// True while a `/goal` autonomous run is actively driving the turn
    /// (goal harness enabled and the durable status is `Active`). Used to
    /// tag background tasks spawned during the goal turn as goal-turn-origin.
    pub(super) fn goal_loop_active(&self) -> bool {
        self.goal_harness_enabled()
            && self.goal_tracker.lock().status()
                == Some(crate::session::goal_tracker::GoalStatus::Active)
    }

    /// Tag `task_id`s the goal model spawned itself during the goal turn as
    /// goal-turn origin (see [`Self::goal_turn_task_ids`]). No-op when the goal
    /// loop isn't active.
    pub(super) fn record_goal_turn_task_ids(&self, task_ids: impl IntoIterator<Item = String>) {
        if !self.goal_loop_active() {
            return;
        }
        let mut set = self.goal_turn_task_ids.lock();
        set.extend(task_ids);
    }

    /// Tag `task_id`s reparented from a harness verifier/planner subagent as
    /// goal-turn origin. Gated on the goal harness being enabled — stable across
    /// status flips — NOT on `Active`, so a final-round skeptic exiting as the
    /// goal flips `Active → Blocked` is still suppressed. The caller already
    /// filters to harness-internal children (`surface_completion: false`).
    pub(super) fn record_reparented_goal_turn_task_ids(
        &self,
        task_ids: impl IntoIterator<Item = String>,
    ) {
        if !self.goal_harness_enabled() {
            return;
        }
        let mut set = self.goal_turn_task_ids.lock();
        set.extend(task_ids);
    }

    pub(super) fn sync_goal_harness(&self) -> bool {
        self.goal_harness_enabled
            .store(self.goal_enabled, std::sync::atomic::Ordering::Relaxed);
        self.goal_enabled
    }

    pub(super) fn sync_goal_harness_from_tools(&self, tool_names: &[String]) -> bool {
        let enabled = goal_slash_and_harness_available(self.goal_enabled, tool_names);
        self.goal_harness_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        enabled
    }

    pub(super) async fn refresh_goal_harness_enabled(&self) {
        if self.goal_runs_on_workflow_engine() {
            self.sync_goal_harness();
        } else {
            let tool_names = self.registered_tool_names().await;
            self.sync_goal_harness_from_tools(&tool_names);
        }
    }

    pub(super) async fn maybe_reconcile_active_goal_without_harness(&self) {
        if self
            .goal_harness_availability_reconciled
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        if self.goal_harness_enabled() {
            return;
        }
        if self.goal_tracker.lock().status()
            != Some(crate::session::goal_tracker::GoalStatus::Active)
        {
            return;
        }
        let _ = self
            .auto_pause_goal_if_active_with_message(
                crate::session::goal_tracker::GoalPauseReason::User,
                "Goal paused: `update_goal` is not available in this session's toolset. \
                 Resume with /goal after the tool is registered."
                    .into(),
            )
            .await;
    }

    /// Load-time safety net: an Active goal restored with `plan_file == None`
    /// (legacy snapshot) is paused with the canonical message.
    /// One-shot via `goal_plan_reconciled`; the mid-session retry path lives
    /// in `resume_goal`, not here.
    pub(super) async fn maybe_reconcile_active_goal_without_plan(&self) {
        if self
            .goal_plan_reconciled
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        if !self.goal_planner_enabled || !self.goal_harness_enabled() {
            return;
        }
        let needs_pause = {
            let tracker = self.goal_tracker.lock();
            match tracker.snapshot() {
                Some(o) => {
                    o.status == crate::session::goal_tracker::GoalStatus::Active
                        && o.plan_file.is_none()
                }
                None => false,
            }
        };
        if !needs_pause {
            return;
        }
        let _ = self
            .auto_pause_goal_if_active_with_message(
                crate::session::goal_tracker::GoalPauseReason::User,
                planner_failure_pause_message(),
            )
            .await;
    }

    pub(super) fn prune_subagent_records_for_active_goal(&self) {
        let goal_id = match self.goal_tracker.lock().snapshot() {
            Some(o) => o.goal_id.clone(),
            None => return,
        };
        self.subagent_token_records
            .lock()
            .retain(|_, r| r.goal_id.as_deref() != Some(goal_id.as_str()));
    }

    /// Run the planner subagent for a goal that has no plan yet.
    /// Called from `setup_goal` (fresh goal) and from `resume_goal`
    /// (post-pause retry, honouring the canonical "Planning failed;
    /// resume with /goal to retry." message). No-op when the planner
    /// is disabled, when the coordinator is not plumbed (tests /
    /// compatible template), or when `plan_file` is already populated.
    /// On `FailClosed` the goal is paused with the canonical reason.
    pub(super) async fn maybe_run_goal_planner(&self, objective: &str) {
        let objective = objective.to_owned();
        let mut steering = Vec::new();
        let run_goal_id = self
            .goal_tracker
            .lock()
            .snapshot()
            .map(|goal| goal.goal_id.clone());
        let _planner_state = GoalPlannerStateGuard {
            tracker: &self.goal_tracker,
        };
        let mut attempt = 0u32;
        loop {
            // Exhausting the retry cap pauses the goal with the canonical
            // message (like any other planner failure), never leaving it Active
            // with no plan.
            if attempt >= GOAL_PLANNER_MAX_ATTEMPTS {
                tracing::debug!(
                    "goal planner: reached max attempts ({GOAL_PLANNER_MAX_ATTEMPTS}); \
                     pausing goal"
                );
                if let Some(goal_id) = run_goal_id.as_deref() {
                    let _ = self
                        .auto_pause_goal_if_matches_with_message(
                            goal_id,
                            crate::session::goal_tracker::GoalPauseReason::User,
                            planner_failure_pause_message(),
                        )
                        .await;
                }
                break;
            }
            attempt += 1;

            let (goal_id, plan_file, attempt_file, outcome) = match self
                .run_goal_planner_attempt(&objective, &steering, run_goal_id.as_deref(), attempt)
                .await
            {
                PlannerAttemptStep::Stop => break,
                PlannerAttemptStep::Steered(extra) => {
                    steering.extend(extra);
                    continue;
                }
                PlannerAttemptStep::Ran {
                    goal_id,
                    plan_file,
                    attempt_file,
                    outcome,
                } => (goal_id, plan_file, attempt_file, outcome),
            };

            // Publish decision. Re-checked after every await so stale planner
            // work never publishes onto a paused/cleared/replaced goal; one
            // predicate keeps the checks below in sync.
            let same_active_goal = |goal: &crate::session::goal_tracker::GoalOrchestration| {
                goal.goal_id == goal_id
                    && goal.status == crate::session::goal_tracker::GoalStatus::Active
            };
            match outcome {
                crate::session::goal_planner::GoalPlannerOutcome::Planned { .. } => {
                    let can_publish = self
                        .goal_tracker
                        .lock()
                        .snapshot()
                        .is_some_and(|goal| same_active_goal(goal) && goal.plan_file.is_none());
                    if !can_publish {
                        break;
                    }
                    // The subagent produced a plan and we are committing to
                    // publish it. `run_goal_planner_attempt` already took the
                    // planner run, so steering can no longer replan — a late
                    // Send Now landing in the publish window is correctly
                    // delivered only as an interjection. Turn the "planning…"
                    // badge off NOW, before the plan/baseline I/O below, instead
                    // of only at the very end, so the UI never advertises
                    // "planning" while it can no longer replan.
                    self.clear_goal_planning_latch(run_goal_id.as_deref()).await;
                    if attempt_file.persist(&plan_file).is_err() {
                        let still_same_goal =
                            self.goal_tracker.lock().snapshot().is_some_and(|goal| {
                                same_active_goal(goal) && goal.plan_file.is_none()
                            });
                        if still_same_goal {
                            let _ = self
                                .auto_pause_goal_if_matches_with_message(
                                    &goal_id,
                                    crate::session::goal_tracker::GoalPauseReason::User,
                                    planner_failure_pause_message(),
                                )
                                .await;
                        }
                        break;
                    }
                    // Record `plan_file`, then snapshot the planner's ORIGINAL
                    // plan as the immutable baseline the verifier diffs later
                    // edits against. Capture once: this runs only when no plan
                    // exists yet, and the `is_none()` guard keeps a restart /
                    // re-entry from overwriting it.
                    let baseline_target = {
                        let mut tracker = self.goal_tracker.lock();
                        let src = tracker.plan_path();
                        let dst = tracker.plan_baseline_path();
                        let Some(goal) = tracker
                            .snapshot_mut()
                            .filter(|goal| same_active_goal(goal) && goal.plan_file.is_none())
                        else {
                            break;
                        };
                        let need_baseline = goal.plan_baseline_file.is_none();
                        goal.plan_file = Some(plan_file);
                        need_baseline.then_some((src, dst))
                    };
                    if let Some((src, dst)) = baseline_target {
                        let tmp = dst
                            .with_file_name(format!("plan-baseline-{}.md", uuid::Uuid::now_v7()));
                        let baseline_file = match tempfile::TempPath::try_from_path(tmp.clone()) {
                            Ok(guard) => guard,
                            Err(err) => {
                                tracing::warn!(
                                    error = %err,
                                    "goal planner: could not stage plan baseline path; \
                                     PLAN_CHANGES will render (none)"
                                );
                                break;
                            }
                        };
                        match tokio::fs::copy(&src, &tmp).await {
                            Ok(_) => {
                                let mut tracker = self.goal_tracker.lock();
                                let can_publish = tracker.snapshot().is_some_and(|goal| {
                                    same_active_goal(goal)
                                        && goal.plan_file.as_deref() == Some(src.as_path())
                                        && goal.plan_baseline_file.is_none()
                                });
                                if can_publish
                                    && baseline_file.persist(&dst).is_ok()
                                    && let Some(goal) = tracker.snapshot_mut()
                                {
                                    goal.plan_baseline_file = Some(dst);
                                }
                            }
                            Err(err) => tracing::warn!(
                                error = %err,
                                src = %src.display(),
                                "goal planner: failed to snapshot plan baseline; \
                                 PLAN_CHANGES will render (none)",
                            ),
                        }
                    }
                }
                crate::session::goal_planner::GoalPlannerOutcome::Interrupted => continue,
                crate::session::goal_planner::GoalPlannerOutcome::FailClosed { .. } => {
                    let _ = self
                        .auto_pause_goal_if_matches_with_message(
                            &goal_id,
                            crate::session::goal_tracker::GoalPauseReason::User,
                            planner_failure_pause_message(),
                        )
                        .await;
                }
            }
            break;
        }

        // Catch-all latch reset for every exit path that did NOT already clear
        // it at the commit-to-publish point (Stop / cap-exhausted / fail-closed /
        // steered-retry, or a publish that broke out before committing). The
        // conditional emit inside the helper keeps the success path's earlier
        // clear from being re-emitted as a duplicate `planning=None`; a no-op if
        // the orchestration has since vanished or the goal was replaced.
        self.clear_goal_planning_latch(run_goal_id.as_deref()).await;
    }

    /// Clear the goal's "planning…" latch for `run_goal_id` and, only if it was
    /// actually set, emit a snapshot-derived `GoalUpdated` so the pager's
    /// planning badge turns off. Returns whether the latch was set (and thus an
    /// emit happened).
    ///
    /// Two call sites in [`Self::maybe_run_goal_planner`] share this: the
    /// commit-to-publish point (once the planner run is taken and a plan is
    /// being published, steering can no longer replan, so the planning phase is
    /// over — turn the badge off BEFORE the publish/baseline I/O) and the
    /// catch-all on every other exit path. The conditional emit keeps the two
    /// sites from double-emitting a redundant `planning=None`.
    async fn clear_goal_planning_latch(&self, run_goal_id: Option<&str>) -> bool {
        let current_tokens = self.chat_state_handle.get_total_tokens().await as i64;
        let (tokens_used, finished_marginal) = self.goal_tokens(current_tokens);
        let mut tracker = self.goal_tracker.lock();
        let cleared = tracker
            .snapshot_mut()
            .filter(|goal| Some(goal.goal_id.as_str()) == run_goal_id)
            .is_some_and(|goal| std::mem::replace(&mut goal.planning_in_flight, false));
        if cleared {
            self.goal_notify_sender().emit_goal_updated(
                &mut tracker,
                tokens_used,
                finished_marginal,
            );
        }
        cleared
    }

    /// Run one planner attempt: re-validate the goal, stage the attempt file,
    /// spawn the planner, and run it to an outcome. Returns a
    /// [`PlannerAttemptStep`] for the loop to act on. Holds no `goal_tracker`
    /// lock across the spawn `.await`.
    async fn run_goal_planner_attempt(
        &self,
        objective: &str,
        steering: &[String],
        run_goal_id: Option<&str>,
        attempt: u32,
    ) -> PlannerAttemptStep {
        if !self.goal_planner_enabled {
            return PlannerAttemptStep::Stop;
        }
        let Some(event_tx) = self.tool_context.subagent_event_tx.clone() else {
            tracing::debug!("goal planner: no subagent coordinator channel; skipping");
            return PlannerAttemptStep::Stop;
        };
        let (goal_id, plan_file, attempt_plan_file) = {
            let tracker = self.goal_tracker.lock();
            match tracker.snapshot() {
                Some(goal)
                    if Some(goal.goal_id.as_str()) != run_goal_id
                        || goal.status != crate::session::goal_tracker::GoalStatus::Active =>
                {
                    return PlannerAttemptStep::Stop;
                }
                Some(goal) if goal.plan_file.is_some() => {
                    tracing::debug!("goal planner: plan already present; skipping");
                    return PlannerAttemptStep::Stop;
                }
                Some(o) => {
                    let plan_file = tracker.plan_path();
                    let attempt_plan_file =
                        plan_file.with_file_name(format!("plan-{}.md", uuid::Uuid::now_v7()));
                    (o.goal_id.clone(), plan_file, attempt_plan_file)
                }
                None => return PlannerAttemptStep::Stop,
            }
        };
        // `TempPath` deletes on drop and persists by rename, so any early exit
        // cleans up the staged file and a failed rename can't leave it behind.
        let attempt_file = match tempfile::TempPath::try_from_path(attempt_plan_file.clone()) {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "goal planner: could not stage attempt plan path; skipping planner"
                );
                return PlannerAttemptStep::Stop;
            }
        };
        let cancel_token = tokio_util::sync::CancellationToken::new();
        self.goal_tracker
            .lock()
            .start_planner_run(cancel_token.clone());

        let attempt_objective = if steering.is_empty() {
            objective.to_owned()
        } else {
            format!("{objective}\n\nUser steering:\n{}", steering.join("\n\n"))
        };

        let model_id = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model)
            .unwrap_or_default();
        // Fork owns history; fail-open stays OBJECTIVE-only (no last-assistant CONTEXT).
        let context = String::new();

        let task_tool_name = self.resolve_goal_tool_names().await.task;
        // Tag the planner with the goal-creation turn's prompt id so its
        // `subagent.json` / parent `subagents_spawned` ref link to this turn,
        // matching how model-spawned subagents attach to their parent.
        let parent_prompt_id = self
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned")
            .clone();
        // Mirror-child forks must use the parent model to reuse its cached prefix.
        let role_override = crate::session::goal_planner::RoleSpawnOverride::default();
        if !matches!(
            self.goal_role_models.planner,
            crate::agent::config::GoalRoleModelChoice::InheritCurrent
        ) {
            tracing::info!(
                "goal planner: configured role model ignored — forced to parent model for verbatim-fork cache reuse"
            );
        }
        let tool_names = self.resolve_inherit_role_tool_names().await;
        let inherit_tool_names = tool_names.clone();
        let spawner: std::sync::Arc<dyn crate::session::goal_planner::GoalPlannerSpawner> =
            std::sync::Arc::new(crate::session::goal_planner::ChannelSpawner {
                event_tx,
                foreground_wait: Some(crate::tools::tool_context::subagent_foreground_wait(
                    self.tool_context.blocking_wait_depth.clone(),
                )),
                parent_session_id: self.session_id_string(),
                parent_prompt_id,
                cwd: Some(self.tool_context.cwd.as_str().to_owned()),
                trace_sink: Some((self.chat_state_handle.clone(), task_tool_name)),
                role_override,
                cancel_token,
                events: Some(self.events.writer()),
            });

        let current_tokens = self.chat_state_handle.get_total_tokens().await as i64;
        self.emit_goal_planning(current_tokens);

        let outcome = crate::session::goal_planner::run_goal_planner(
            spawner,
            crate::session::goal_planner::GoalPlannerInputs {
                objective: &attempt_objective,
                context: &context,
                plan_file: &attempt_plan_file,
                attempt,
                model_id: crate::session::goal_planner::effective_role_model_id(None, &model_id),
                tool_names: &tool_names,
                inherit_tool_names: &inherit_tool_names,
            },
            &|e| self.events.emit(e),
        )
        .await;

        // Seal the planner's synthetic `task` pair into its own harness trace
        // turn so it uploads as a sibling `turn_{N}` artifact (the planner is
        // represented by its own turn). No-op when the spawn recorded nothing.
        self.chat_state_handle.flush_harness_trace_turn();

        let planner_state = self.goal_tracker.lock().take_planner_run();
        if let Some(state) = planner_state
            && !state.steering.is_empty()
        {
            return PlannerAttemptStep::Steered(state.steering);
        }

        PlannerAttemptStep::Ran {
            goal_id,
            plan_file,
            attempt_file,
            outcome,
        }
    }

    /// Run the stall-triggered strategist subagent (best-effort, fail-OPEN).
    /// Called from `apply_classifier_outcome`'s `NotAchieved` branch once the
    /// consecutive-failure streak hits a multiple of `goal_strategist_every`
    /// and neither the cap nor the stall paused the round. On success the
    /// recommendation + strategy-note path are persisted on the orchestration
    /// so the continuation directive can inline them. Any failure (no
    /// coordinator, spawn error, missing note) is logged and ignored — the
    /// goal keeps running, never pauses. No `goal_tracker` lock is held across
    /// the strategist `.await`.
    pub(super) async fn maybe_run_goal_strategist(&self, attempt: u32, consecutive_failures: u32) {
        // The claim granted the cap bonus up front; every exit that delivers
        // no restructure (early return, FailOpen, future dropped by a turn
        // cancel) must give it back.
        let mut bonus_guard = TrackerDropGuard::new(&self.goal_tracker, |t| {
            t.revoke_strategist_cap_bonus();
        });
        let Some(event_tx) = self.tool_context.subagent_event_tx.clone() else {
            tracing::debug!("goal strategist: no subagent coordinator channel; skipping");
            return;
        };

        // Assemble inputs under one scoped lock, then drop it before the spawn
        // await. `plan_path` / `strategy_path` are derived from the tracker
        // while we hold the lock (cheap path joins).
        let (objective, plan_file, strategy_file, verifier_id) = {
            let tracker = self.goal_tracker.lock();
            let Some(o) = tracker.snapshot() else {
                return;
            };
            (
                o.objective.clone(),
                tracker.plan_path(),
                tracker.strategy_path(),
                o.verifier_id.clone(),
            )
        };

        // The strategist reads the run's traces itself (transcript + verdict
        // history) instead of a pre-assembled gaps/diff packet.
        let session_traces_dir = crate::session::persistence::session_dir(&self.session_info);
        let scratch_root = crate::session::goal_tracker::goal_scratch_root(&verifier_id);

        let model_id = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model)
            .unwrap_or_default();

        let task_tool_name = self.resolve_goal_tool_names().await.task;
        let parent_prompt_id = self
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned")
            .clone();
        // Resolve the strategist role override: entitlement +
        // toolset capability gate, per-role fail-open. Borrows `event_tx`
        // for the describe round-trip; `event_tx` moves into the spawner.
        let (role_override, tool_names, inherit_tool_names) = self
            .resolve_goal_single_role_override(
                "strategist",
                &self.goal_role_models.strategist,
                goal::RoleCapability::Strategist,
                &event_tx,
            )
            .await;
        // Cloned for telemetry before `role_override` moves into the spawner.
        let strategist_model_override = role_override.model.clone();
        let spawner: std::sync::Arc<dyn crate::session::goal_strategist::GoalStrategistSpawner> =
            std::sync::Arc::new(crate::session::goal_strategist::ChannelSpawner {
                event_tx,
                foreground_wait: Some(crate::tools::tool_context::subagent_foreground_wait(
                    self.tool_context.blocking_wait_depth.clone(),
                )),
                parent_session_id: self.session_id_string(),
                parent_prompt_id,
                cwd: Some(self.tool_context.cwd.as_str().to_owned()),
                trace_sink: Some((self.chat_state_handle.clone(), task_tool_name)),
                role_override,
                events: Some(self.events.writer()),
            });

        let outcome = crate::session::goal_strategist::run_goal_strategist(
            spawner,
            crate::session::goal_strategist::GoalStrategistInputs {
                objective: &objective,
                plan_file: &plan_file,
                strategy_file: &strategy_file,
                session_traces_dir: &session_traces_dir,
                scratch_root: &scratch_root,
                attempt,
                consecutive_failures,
                every: self.goal_strategist_every,
                model_id: crate::session::goal_planner::effective_role_model_id(
                    strategist_model_override.as_deref(),
                    &model_id,
                ),
                tool_names: &tool_names,
                inherit_tool_names: &inherit_tool_names,
            },
            &|e| self.events.emit(e),
        )
        .await;

        // Seal the strategist's synthetic `task` pair into its own harness
        // trace turn (sibling of the planner / skeptic turns). No-op when the
        // spawn recorded nothing.
        self.chat_state_handle.flush_harness_trace_turn();

        // Fail-OPEN: persist on success; any other exit leaves `bonus_guard`
        // armed (the runner already emitted telemetry + warning).
        if let crate::session::goal_strategist::GoalStrategistOutcome::Advised {
            strategy_file,
            recommendation,
            ..
        } = outcome
        {
            self.goal_tracker.lock().record_strategy_recommendation(
                strategy_file.to_string_lossy().into_owned(),
                recommendation,
            );
            bonus_guard.disarm();
        }
    }

    /// Generate the ONE closing user-facing summary after a goal is
    /// verified-achieved and surface it as the goal turn's final message.
    ///
    /// Best-effort / fail-OPEN: gated by `goal_summary_enabled`; any failure
    /// (disabled, no coordinator, spawn error, empty output) is logged /
    /// telemetry'd and skipped — goal completion is NEVER blocked, paused, or
    /// un-achieved (the goal is already Complete when this runs). Read-only:
    /// the spawn pins a read-only toolset and the prompt forbids edits. No
    /// `goal_tracker` lock is held across the summarizer `.await`.
    pub(super) async fn maybe_run_goal_summarizer(&self, attempt: u32) {
        if !self.goal_summary_enabled {
            return;
        }
        let Some(event_tx) = self.tool_context.subagent_event_tx.clone() else {
            tracing::debug!("goal summarizer: no subagent coordinator channel; skipping");
            return;
        };

        // Snapshot inputs under one scoped lock, dropped before the awaits.
        let (objective, plan_file, details_file) = {
            let tracker = self.goal_tracker.lock();
            let Some(o) = tracker.snapshot() else {
                return;
            };
            (
                o.objective.clone(),
                tracker.plan_path(),
                o.last_classifier_details_path.clone(),
            )
        };

        let session_traces_dir = crate::session::persistence::session_dir(&self.session_info);
        let model_id = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model)
            .unwrap_or_default();
        let task_tool_name = self.resolve_goal_tool_names().await.task;
        let parent_prompt_id = self
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned")
            .clone();
        // The summarizer always inherits the current model (no per-role key);
        // its §7 prompt names the parent toolset's tools.
        let tool_names = self.resolve_inherit_role_tool_names().await;

        let spawner: std::sync::Arc<dyn crate::session::goal_summarizer::GoalSummarizerSpawner> =
            std::sync::Arc::new(crate::session::goal_summarizer::ChannelSpawner {
                event_tx,
                foreground_wait: Some(crate::tools::tool_context::subagent_foreground_wait(
                    self.tool_context.blocking_wait_depth.clone(),
                )),
                parent_session_id: self.session_id_string(),
                parent_prompt_id,
                cwd: Some(self.tool_context.cwd.as_str().to_owned()),
                trace_sink: Some((self.chat_state_handle.clone(), task_tool_name)),
                events: Some(self.events.writer()),
            });

        let outcome = crate::session::goal_summarizer::run_goal_summarizer(
            spawner,
            crate::session::goal_summarizer::GoalSummarizerInputs {
                objective: &objective,
                plan_file: &plan_file,
                details_file: details_file.as_deref(),
                session_traces_dir: &session_traces_dir,
                attempt,
                model_id: &model_id,
                tool_names: &tool_names,
            },
            &|e| self.events.emit(e),
        )
        .await;

        // Seal the summarizer's synthetic `task` pair into its own harness
        // trace turn (sibling of the planner / skeptic / strategist turns).
        self.chat_state_handle.flush_harness_trace_turn();

        // Fail-OPEN: surface the summary on success; any other outcome already
        // emitted telemetry and is skipped. The chunk persists via
        // `updates.jsonl`, so resume/rewind keep it.
        if let crate::session::goal_summarizer::GoalSummarizerOutcome::Summarized {
            summary, ..
        } = outcome
        {
            // Bump the stream start so the summary chunk carries a fresh
            // `streamStartMs`; without it the client appends this closing
            // message to the model's last turn message instead of a new block.
            self.chat_state_handle
                .record_stream_start(chrono::Utc::now().timestamp_millis());
            self.send_slash_command_output(&summary).await;
        }
    }

    /// Build a [`GoalNotifySender`] for the goal orchestrator.
    ///
    /// The sender can emit notifications and push system reminders
    /// independently of `SessionActor` (used by the `spawn_local`
    /// orchestrator task).
    pub(crate) fn goal_notify_sender(&self) -> crate::session::goal_orchestrator::GoalNotifySender {
        crate::session::goal_orchestrator::GoalNotifySender::new(
            self.session_info.id.clone(),
            self.notifications.gateway.clone(),
            self.notifications.persistence_tx.clone(),
        )
    }

    /// Returns `(ratcheted_total, finished_subagent_marginal_sum)` for the
    /// active goal, or `(0, 0)` if no orchestration is loaded.
    ///
    /// The ratcheted total folds in EVERY goal-scoped subagent's marginal
    /// (finished + in-flight) so the displayed/enforced spend tracks live
    /// progress. The second value is the wire `finished_subagent_tokens`
    /// and intentionally folds ONLY sealed (`finished`) records: the pager
    /// adds its own live active-subagent sum on top of that field
    /// (`GoalDisplayState::live_tokens_used`), so including an in-flight
    /// subagent here would double-count it in the live display / budget bar.
    ///
    /// Parent usage is accumulated as a monotonic spend counter
    /// (`parent_tokens_spent`): only POSITIVE deltas of the session token
    /// total are added, anchored at `last_session_tokens_seen`. A
    /// compaction that shrinks the context total merely re-anchors, so
    /// the count can neither decrease nor freeze until context regrows
    /// past a prior peak. Best-effort sampling: growth fully consumed by
    /// a compaction between two calls is unobserved.
    ///
    /// Side-effect: advances the spend accumulator and ratchets
    /// `tokens_used_high_water` monotonically; idempotent under stable
    /// inputs.
    pub(crate) fn goal_tokens(&self, current_session_tokens: i64) -> (i64, i64) {
        let goal_id = {
            let tracker = self.goal_tracker.lock();
            match tracker.snapshot() {
                Some(o) => o.goal_id.clone(),
                None => return (0, 0),
            }
        };
        // `subagent_sum` folds every goal-scoped record (finished + in-flight)
        // into the ratcheted total; `finished_subagent_sum` folds only sealed
        // records and is what ships on the wire as `finished_subagent_tokens`.
        // Keeping them distinct preserves the pager contract — the pager sums
        // running subagents itself and adds them to `finished_subagent_tokens`,
        // so an in-flight marginal must NOT appear in the wire field.
        let (subagent_sum, finished_subagent_sum) = {
            let records = self.subagent_token_records.lock();
            records
                .values()
                .filter(|r| r.goal_id.as_deref() == Some(goal_id.as_str()))
                .fold((0i64, 0i64), |(all, finished), r| {
                    let d = r.marginal();
                    if i64::try_from(d).is_err() {
                        static WARNED: std::sync::Once = std::sync::Once::new();
                        WARNED.call_once(|| {
                            tracing::warn!(
                                marginal = d,
                                "subagent token marginal exceeds i64::MAX; saturating"
                            );
                        });
                    }
                    let d = i64::try_from(d).unwrap_or(i64::MAX);
                    let all = all.saturating_add(d);
                    let finished = if r.finished {
                        finished.saturating_add(d)
                    } else {
                        finished
                    };
                    (all, finished)
                })
        };
        let ratcheted = {
            let mut tracker = self.goal_tracker.lock();
            let o = tracker
                .snapshot_mut()
                .expect("orchestration vanished between locks on single-threaded actor");
            // Legacy snapshots carry no anchor; seed from the goal baseline.
            let last_seen = o.last_session_tokens_seen.unwrap_or(o.token_baseline);
            if current_session_tokens > last_seen {
                o.parent_tokens_spent = o
                    .parent_tokens_spent
                    .saturating_add(current_session_tokens - last_seen);
            }
            o.last_session_tokens_seen = Some(current_session_tokens);
            let raw = o.parent_tokens_spent.saturating_add(subagent_sum);
            o.tokens_used_high_water = o.tokens_used_high_water.max(raw);
            o.tokens_used_high_water
        };
        (ratcheted, finished_subagent_sum)
    }

    /// Thin wrapper around [`Self::goal_tokens`] returning only the
    /// ratcheted total. Inherits the high-water-mark side-effect.
    pub(crate) fn goal_tokens_used(&self, current_session_tokens: i64) -> i64 {
        self.goal_tokens(current_session_tokens).0
    }

    /// Per-model marginal-token breakdown for the active goal's LIVE
    /// active-subagent window, sorted by tokens descending. Empty when no
    /// goal is loaded. Records with no captured model fold under
    /// `current_model_id` (best-effort). See [`fold_tokens_by_model`] for the
    /// fold contract. Fed into `update_live_progress` by the
    /// `SubagentProgress` handler.
    ///
    /// Sealed (`finished`) records are excluded: this feeds
    /// `live_tokens_by_model`, which is rendered under the running subagent's
    /// live block, so spend from earlier finished subagents must not leak in.
    /// This is the per-model analogue of the finished/in-flight split in
    /// [`Self::goal_tokens`]; the ratcheted total path is unaffected.
    pub(crate) fn goal_tokens_by_model(&self, current_model_id: &str) -> Vec<(String, u64)> {
        let goal_id = match self.goal_tracker.lock().snapshot() {
            Some(o) => o.goal_id.clone(),
            None => return Vec::new(),
        };
        let records = self.subagent_token_records.lock();
        fold_tokens_by_model(
            records.values().filter(|r| !r.finished),
            &goal_id,
            current_model_id,
        )
    }

    /// Push the tool-layer `GoalLoopActive` flag so per-tool-call bg-task /
    /// subagent completion reminders suppress themselves while the goal loop
    /// drives the turn. Mirrors the `CurrentPromptIdResource` push.
    ///
    /// Also mirrors the value into `tool_context.goal_loop_active_gate`, the
    /// shared `Arc<AtomicBool>` the notification bridge (bash auto-wake) and
    /// subagent spawn contexts (subagent auto-wake) read to suppress synthetic
    /// completion prompts mid-goal. Writing both from this one chokepoint keeps
    /// the gate from *persistently* drifting from the resource; the two writes
    /// are sequential (gate store, then async `update_resource`), so a transient
    /// window exists — benign, since those consumers read only the gate.
    pub(super) async fn set_goal_loop_active_resource(&self, active: bool) {
        self.tool_context
            .goal_loop_active_gate
            .store(active, std::sync::atomic::Ordering::Relaxed);
        self.agent
            .borrow()
            .tool_bridge()
            .update_resource(
                echo_agent_tools::implementations::echo_agent_build::task::types::GoalLoopActive(
                    active,
                ),
            )
            .await;
    }
}
