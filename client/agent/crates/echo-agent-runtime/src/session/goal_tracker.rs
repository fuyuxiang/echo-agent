//! Goal mode state machine.
//!
//! This module contains [`GoalTracker`], a pure state machine (no async I/O)
//! modeled after [`PlanModeTracker`](super::plan_mode::PlanModeTracker).
//! The `SessionActor` owns one `GoalTracker` behind a `Mutex` and calls
//! its methods at the appropriate orchestration points.
//!
//! Persisted `"infra_paused"` requires this shell version (one-way upgrade).
//! Unknown wire values (including unknown `*_paused` forms) deserialize to
//! [`GoalStatus::UserPaused`] so a corrupt or forward-version snapshot can
//! never resurrect as a self-driving goal.

use std::path::PathBuf;
use std::time::Instant;

/// Consecutive identical gap fingerprints that trip the stall
/// early-exit: two in a row means the model produced no change in the
/// flagged gaps between attempts, so iterating further is futile and
/// the goal auto-pauses before exhausting the run cap.
pub(crate) const GOAL_CLASSIFIER_STALL_THRESHOLD: u32 = 2;

/// Extra classifier rounds granted (once) when the strategist fires, so its
/// restructure isn't starved under a small cap.
pub(crate) const GOAL_STRATEGIST_CAP_BONUS: u32 = 3;

/// Relaxed stall threshold while a strategist restructure is in flight (cap
/// bonus active): sized to cover the granted bonus rounds, yet bounded so a
/// stuck restructure still exits.
pub(crate) const GOAL_STRATEGIST_STALL_THRESHOLD: u32 =
    GOAL_CLASSIFIER_STALL_THRESHOLD + GOAL_STRATEGIST_CAP_BONUS;

/// Max retained goal-history entries. Only the last is surfaced on the wire
/// (`GoalUpdated.last_event`), but the whole list is persisted, so it is
/// capped (oldest dropped) to keep a long goal's snapshot bounded.
const GOAL_HISTORY_MAX: usize = 64;

// Phase / Status enums

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalPhase {
    Idle,
    Planning,
    Executing,
}

/// Lifecycle status of a goal. The paused variants encode the
/// reason the goal was paused — `UserPaused` for Ctrl+C / `/goal pause`,
/// `BackOffPaused` when the classifier run cap is hit, `NoProgressPaused`
/// when the verifier flags the same gaps with no progress before the cap,
/// `InfraPaused` when a turn finishes with an infrastructure error,
/// and `Blocked` when the model determined the goal is not achievable
/// in the current environment. Use [`GoalStatus::is_paused`] to test
/// paused-ness uniformly across all six variants.
///
/// **Backwards-compat serde aliases:** older shells serialized this
/// enum with the default PascalCase form (`"Active"`, `"Paused"`,
/// `"BudgetLimited"`, `"Complete"`). The `#[serde(alias = ...)]`
/// attributes preserve in-flight goal snapshots written by older shells
/// — legacy `"Paused"` maps to `UserPaused` (matches the pager-side
/// fallback). New
/// snapshots emit snake_case per `rename_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[serde(alias = "Active")]
    Active,
    #[serde(alias = "Paused")]
    UserPaused,
    BackOffPaused,
    /// Verifier flagged the same gaps across consecutive attempts (no
    /// progress) and auto-paused before the run cap. Resumable, same paused
    /// family as `BackOffPaused`; split out so the UI distinguishes a stall
    /// from a cap pause.
    NoProgressPaused,
    /// Infrastructure turn failure (`PromptTurnResult::Err`). The
    /// human-readable reason is stashed in [`GoalOrchestration::pause_message`].
    InfraPaused,
    /// stashed in [`GoalOrchestration::pause_message`].
    Blocked,
    #[serde(alias = "BudgetLimited")]
    BudgetLimited,
    #[serde(alias = "Complete")]
    Complete,
}

impl<'de> serde::Deserialize<'de> for GoalStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_wire_str(&s))
    }
}

impl GoalStatus {
    /// Parse a persisted/wire status string. Unknown values map to
    /// `UserPaused`: a status this shell cannot interpret must restore as
    /// a resumable paused goal, never an Active self-driving one.
    pub fn from_wire_str(s: &str) -> Self {
        match s {
            "active" | "Active" => Self::Active,
            "user_paused" | "paused" | "Paused" => Self::UserPaused,
            // Historical status from shells that had doom-loop auto-pause.
            "doom_loop_paused" => Self::UserPaused,
            "back_off_paused" => Self::BackOffPaused,
            "no_progress_paused" => Self::NoProgressPaused,
            "infra_paused" => Self::InfraPaused,
            "blocked" => Self::Blocked,
            "budget_limited" | "BudgetLimited" => Self::BudgetLimited,
            "complete" | "Complete" => Self::Complete,
            _ => Self::UserPaused,
        }
    }
    /// `true` for any paused variant (`UserPaused`, `BackOffPaused`,
    /// `NoProgressPaused`, `InfraPaused`, `Blocked`).
    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            Self::UserPaused
                | Self::BackOffPaused
                | Self::NoProgressPaused
                | Self::InfraPaused
                | Self::Blocked
        )
    }
}

/// Input to [`GoalTracker::pause`] / [`GoalTracker::pause_with_message`]
/// and the auto-pause helpers. Maps 1:1 to one of the paused variants on
/// [`GoalStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalPauseReason {
    User,
    BackOff,
    /// Verification stage saw no change in the flagged-gap fingerprint
    /// across consecutive attempts and auto-paused before the run cap.
    /// Maps to [`GoalStatus::NoProgressPaused`] — same resumable paused
    /// family as the cap, surfaced distinctly in the UI / telemetry.
    NoProgress,
    /// [`GoalStatus::Blocked`]; pairs with a human-readable message on
    /// [`GoalOrchestration::pause_message`].
    Verification,
    /// Turn finished with `PromptTurnResult::Err`. Maps to
    /// [`GoalStatus::InfraPaused`]; pairs with a human-readable message on
    /// [`GoalOrchestration::pause_message`].
    Infra,
}

impl GoalPauseReason {
    fn to_status(self) -> GoalStatus {
        match self {
            Self::User => GoalStatus::UserPaused,
            Self::BackOff => GoalStatus::BackOffPaused,
            Self::NoProgress => GoalStatus::NoProgressPaused,
            Self::Verification => GoalStatus::Blocked,
            Self::Infra => GoalStatus::InfraPaused,
        }
    }

    /// Short, stable label stashed in the `GoalPaused` history entry's
    /// `detail` so the pager's Recent History distinguishes pause causes.
    fn history_detail(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::BackOff => "back_off",
            Self::NoProgress => "no_progress",
            Self::Verification => "blocked",
            Self::Infra => "infra",
        }
    }
}

/// Aggregate verdict produced by the goal-verification stage.
/// `Achieved` indicates the adversarial skeptic panel judged the goal
/// complete; `NotAchieved` means another worker round is warranted.
/// Serialized in snake_case to match `GoalStatus` / `GoalPhase`. The
/// enum name retains the `Classifier` prefix for wire stability across
/// the verification-stage rewire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalClassifierVerdict {
    Achieved,
    NotAchieved,
}

// History

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalEvent {
    GoalCreated,
    PlanningStarted,
    PlanningCompleted,
    PlanningFailed,
    WorkerStarted,
    WorkerCompleted,
    WorkerFailed,
    ContextRotated,
    GoalPaused,
    GoalResumed,
    GoalCompleted,
    GoalCleared,
    BudgetExceeded,
    /// The model tried to stop early (a "giving up"-style bail) while the
    /// goal still had open work and the harness re-nudged it. `detail`
    /// carries the matched stop-pattern label.
    PrematureStopDetected,
    /// Forward-compat sink: a history event written by a newer shell that
    /// this binary doesn't know. Lets an older binary deserialize a newer
    /// snapshot's history instead of failing the whole field.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoalHistoryEntry {
    pub timestamp: String,
    pub event: GoalEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmet: Vec<String>,
}

impl GoalHistoryEntry {
    /// Minimal lifecycle entry stamped with the current time (`round` /
    /// `tokens_used` / `unmet` left empty). Single constructor so the
    /// timestamp + defaults aren't re-spelled at every call site.
    pub(crate) fn now(event: GoalEvent, detail: Option<String>) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event,
            detail,
            round: None,
            tokens_used: None,
            unmet: Vec::new(),
        }
    }
}

// GoalOrchestration (full persisted state)

/// Generate a short opaque identifier used to scope the per-goal
/// scratch root (`<temp_dir>/echo-agent-goal-<id>`) and the verifier
/// verdict/details files inside it.
///
/// The id is a 12-char prefix of a UUIDv4 simple form — ~48 bits of
/// entropy, enough to avoid collision between concurrent goals on the
/// same machine while staying short enough that the orchestrator model
/// can copy it verbatim into spawned verifier prompts without
/// truncation or typo risk (see the past-issue memory note on
/// "UUID-in-prompt copy-fidelity failure mode").
pub(crate) fn generate_verifier_id() -> String {
    let mut s = uuid::Uuid::new_v4().simple().to_string();
    s.truncate(12);
    s
}

/// Private per-goal scratch root: `<temp_dir>/echo-agent-goal-<verifier_id>`.
///
/// Rooted at [`std::env::temp_dir`] (respects `TMPDIR`) and namespaced by the
/// goal's `verifier_id`, so concurrent goals never collide and cleanup of one
/// never touches another. Removed wholesale on every terminal goal transition.
pub(crate) fn goal_scratch_root(verifier_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("echo-agent-goal-{verifier_id}"))
}

/// Create (or verify) the goal's scratch root, locked to the owner
/// (0700 on unix). Artifact names under it are predictable from the
/// prompt/log-visible `verifier_id`, so the root itself is the
/// symlink/squat defense: creation is atomic with mode 0700 (no
/// default-mode window), and a pre-existing entry is accepted only if
/// [`verify_owned_real_dir`] passes, then re-pinned to 0700 (safe to
/// chmod: the entry is proven ours and the sticky-bit temp parent
/// prevents swapping it). Callers treat `Err` as "do not write
/// classifier artifacts here".
pub(crate) fn ensure_goal_scratch_root(verifier_id: &str) -> std::io::Result<PathBuf> {
    let root = goal_scratch_root(verifier_id);
    #[cfg(unix)]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(&root) {
        Ok(()) => Ok(root),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_owned_real_dir(&root)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(root)
        }
        Err(e) => Err(e),
    }
}

/// Shared squat predicate: `Ok` iff `path` is a REAL directory
/// (`symlink_metadata`, never follows) owned by the current euid.
fn verify_owned_real_dir(path: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other(
            "goal scratch root exists but is not a real directory (symlink squat?)",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::other(
                "goal scratch root exists but is not owned by the current user",
            ));
        }
    }
    Ok(())
}

/// Cross-filesystem rescue copy; `create_new` (O_EXCL) makes any
/// pre-existing destination — symlink included — fail the copy instead
/// of being written through.
fn copy_no_follow(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    let mut src_f = std::fs::File::open(src)?;
    let mut dest_f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    std::io::copy(&mut src_f, &mut dest_f)?;
    Ok(())
}

/// Parse `(attempt, skeptic_idx)` out of a per-skeptic report file name
/// (`goal-classifier-{vid}-{attempt}-skeptic-{idx}.md`); `None` otherwise.
fn parse_skeptic_report_order(name: &str) -> Option<(u32, u32)> {
    let stem = name.strip_suffix(".md")?;
    let (head, idx) = stem.rsplit_once("-skeptic-")?;
    let (_, attempt) = head.rsplit_once('-')?;
    Some((attempt.parse().ok()?, idx.parse().ok()?))
}

/// Inline the per-skeptic reports below the just-rescued canonical
/// details file — its path references die with the scratch root, so the
/// rescued file must be self-contained.
///
/// Numeric (attempt DESC, skeptic ASC) order so budget elision drops
/// stale attempts, never the final attempt the canonical body
/// references. Whole files only: each report's on-disk size is checked
/// against the remaining budget BEFORE reading (reports are
/// harness-uncapped and this runs under the tracker lock); the first
/// overflow writes an elision marker and stops. Best-effort.
fn append_skeptic_reports(scratch_root: &std::path::Path, dest: &std::path::Path) {
    use std::io::Write;

    let Ok(entries) = std::fs::read_dir(scratch_root) else {
        return;
    };
    let mut reports: Vec<((u32, u32), PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let (attempt, idx) =
                parse_skeptic_report_order(path.file_name()?.to_string_lossy().as_ref())?;
            Some(((attempt, idx), path))
        })
        .collect();
    reports.sort_by_key(|&((attempt, idx), _)| (std::cmp::Reverse(attempt), idx));
    if reports.is_empty() {
        return;
    }
    let Ok(mut out) = std::fs::OpenOptions::new().append(true).open(dest) else {
        return;
    };
    let mut budget = crate::session::goal_classifier::GOAL_VERIFIER_PANEL_MAX_BYTES as u64;
    for (_, report) in reports {
        let name = report.file_name().unwrap_or_default().to_string_lossy();
        let header = format!("\n\n---\n## Inlined skeptic report: {name}\n\n");
        let Ok(len) = std::fs::symlink_metadata(&report).map(|m| m.len()) else {
            continue;
        };
        if header.len() as u64 + len > budget {
            let _ = out
                .write_all(b"\n\n---\n(remaining skeptic reports elided: rescue budget reached)\n");
            return;
        }
        let Ok(body) = std::fs::read_to_string(&report) else {
            continue;
        };
        budget = budget.saturating_sub(header.len() as u64 + body.len() as u64);
        if out
            .write_all(header.as_bytes())
            .and_then(|()| out.write_all(body.as_bytes()))
            .is_err()
        {
            return;
        }
    }
}

/// The goal model's private scratch dir (`<scratch_root>/implementer`).
/// The implementer writes screenshots, temp scripts, and throwaway
/// artifacts here; the skeptics READ it to verify the claimed outputs.
pub(crate) fn implementer_scratch_dir(verifier_id: &str) -> PathBuf {
    goal_scratch_root(verifier_id).join("implementer")
}

/// Skeptic `idx`'s private scratch dir (`<scratch_root>/skeptic-<idx>`).
/// Each skeptic re-runs the verification plan into its OWN dir so N
/// skeptics never overwrite each other or the implementer's outputs.
pub(crate) fn skeptic_scratch_dir(verifier_id: &str, idx: u32) -> PathBuf {
    goal_scratch_root(verifier_id).join(format!("skeptic-{idx}"))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoalOrchestration {
    pub goal_id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub phase: GoalPhase,
    pub token_budget: Option<i64>,
    pub elapsed_ms: u64,
    pub created_at: String,
    pub current_subagent_id: Option<String>,
    pub current_subagent_role: Option<String>,
    #[serde(default)]
    pub total_worker_rounds: u32,
    #[serde(default)]
    pub total_verify_rounds: u32,
    #[serde(skip)]
    pub budget_limit_reported: bool,
    /// Session-wide total tokens recorded at goal creation. Seeds the
    /// spend accumulator (`last_session_tokens_seen`) so pre-goal usage
    /// is excluded from the goal's token count.
    #[serde(default)]
    pub token_baseline: i64,
    /// Monotonic high-water mark ratcheted by `SessionActor::goal_tokens`
    /// so wire values never decrease across compactions.
    #[serde(default)]
    pub tokens_used_high_water: i64,
    /// Cumulative parent-session tokens spent on this goal: the sum of
    /// positive per-call deltas of the session token total. Unlike a
    /// `current - baseline` difference, this can never shrink or freeze
    /// when auto-compaction reduces the context-size total. Best-effort
    /// sampling: growth fully consumed by a compaction between two
    /// `goal_tokens` calls is unobserved, and spend accrued since the
    /// last persisted snapshot is lost on crash (bounded by the snapshot
    /// cadence).
    #[serde(default)]
    pub parent_tokens_spent: i64,
    /// Session token total at the previous `SessionActor::goal_tokens`
    /// call — the anchor for the next positive delta. `None` on legacy
    /// snapshots; seeded from `token_baseline` on first use.
    #[serde(default)]
    pub last_session_tokens_seen: Option<i64>,
    pub history: Vec<GoalHistoryEntry>,

    /// Human-readable explanation set when the goal transitions to a
    /// paused state with a meaningful reason. `Blocked` and `InfraPaused`
    /// populate it (via [`GoalTracker::pause_with_message`]),
    /// but the field is orthogonal to status so future reasons can
    /// reuse it. Set only by [`GoalTracker::pause_with_message`];
    /// cleared by every transition out of a paused state —
    /// [`GoalTracker::resume`], [`GoalTracker::complete`], and
    /// [`GoalTracker::budget_limit`] all reset it to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_blocker_key: Option<String>,
    #[serde(default)]
    pub evaluator_blocked_streak: u32,

    /// Short opaque identifier used to scope per-goal artifact paths
    /// owned by the harness. Today's consumers:
    ///
    /// * `goal_classifier.rs` — classifier details / changes diff /
    ///   per-skeptic verdict files (see the
    ///   `GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`,
    ///   `GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE`,
    ///   `GOAL_VERIFIER_VERDICT_PATH_TEMPLATE`,
    ///   `GOAL_VERIFIER_DETAILS_PATH_TEMPLATE` consts).
    ///
    /// Generated by [`generate_verifier_id`] when the goal is created
    /// and persisted alongside the rest of the orchestration so the
    /// same id is reused across pause/resume cycles. The current
    /// model-facing template no longer references this id; the model
    /// is no longer instructed to read per-goal verdict files.
    ///
    /// Older persisted snapshots predate this field; the `serde(default)`
    /// attribute backfills a fresh id on load so verdict-file paths
    /// stay well-formed even after an upgrade.
    #[serde(default = "generate_verifier_id")]
    pub verifier_id: String,

    /// Number of times the goal-achievement classifier has been run
    /// for this goal. Reset only when the goal is recreated.
    #[serde(default)]
    pub classifier_runs_attempted: u32,
    /// Worker rounds since the last verification fired: `+1` per
    /// continuation build, reset to 0 when a classifier attempt is
    /// reserved. Drives the re-verify escalation.
    #[serde(default)]
    pub rounds_since_verify: u32,
    /// Hard cap on classifier runs for this goal. `None` means the
    /// cap has not been configured; `Some(0)` reserves the explicit
    /// "zero runs allowed" case. Mirrors the `token_budget:
    /// Option<i64>` precedent on this struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_max_runs: Option<u32>,
    /// Last aggregate verdict returned by the verification stage, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_classifier_verdict: Option<GoalClassifierVerdict>,
    /// Path to the most recent verification-stage details artifact on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_classifier_details_path: Option<String>,
    /// RFC3339 timestamp of the last classifier run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_classifier_at: Option<String>,
    /// Curated per-refuter gap summary (`build_gaps_summary`) from the
    /// most recent `NotAchieved` verdict. Inlined verbatim into every
    /// continuation directive until a later verdict overwrites it (an
    /// `Achieved` verdict clears it), so the freshest verifier feedback
    /// reaches the model each round rather than once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_classifier_gaps: Option<String>,
    /// First verification round's full `FINAL_RESPONSE`, replayed as the
    /// breadth anchor on later rounds so a cold skeptic panel sees the whole
    /// deliverable, not just that round's fix note. Captured once (capped);
    /// never cleared on `Achieved` — it must outlive each round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_final_response: Option<String>,
    /// Child session id of skeptic 0 (the persistent reject-gatekeeper)
    /// from the most recent N > 1 verification attempt. The next attempt
    /// resumes it (`resume_from`) so it delta-re-checks the prior gaps
    /// instead of re-analyzing cold. Cleared by [`GoalTracker::from_snapshot`]
    /// (the in-memory token records that anchor a resumed child's marginal
    /// accounting do not survive a restart), on goal completion, and never
    /// set for an N == 1 sole-judge panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeptic0_session_id: Option<String>,
    /// Resolved skeptic index → `{model, agent_type}` assignment, frozen at
    /// the first verification panel and reused on every resume so skeptic-0
    /// (and the cold panel) keep stable models across attempts. Index `i`
    /// holds `pool[i % pool.len()]`; the vector grows (clamped) but never
    /// rewrites a committed index. Empty ⇒ all skeptics inherit the current
    /// model. Persists across snapshot save/restore exactly like
    /// `skeptic0_session_id`, and is reset on the same terminal transitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skeptic_model_assignment: Vec<crate::util::config::GoalRoleModel>,
    /// Normalized gap fingerprint of the previous `NotAchieved`
    /// rejection (see `goal_classifier::gap_fingerprint`). Compared
    /// against the next rejection's fingerprint to detect a stuck loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_fingerprint: Option<String>,
    /// Count of consecutive rejections carrying the same gap
    /// fingerprint (1 on the first occurrence of a fingerprint). Drives
    /// the stall early-exit in [`GoalTracker::record_classifier_stall`].
    #[serde(default)]
    pub classifier_stall_count: u32,
    /// Count of consecutive `NotAchieved` verifications regardless of
    /// gap content (resets to 0 on an `Achieved` verdict). Drives the
    /// stall-triggered strategist: it fires when this reaches the
    /// configured `goal_strategist_every` (N) and again at each multiple
    /// (2N, 3N, …). Distinct from `classifier_stall_count`, which only
    /// counts *identical*-fingerprint repeats — this catches whack-a-mole
    /// where each round flags a different gap.
    #[serde(default)]
    pub consecutive_not_achieved: u32,
    /// The `consecutive_not_achieved` value at which the strategist last
    /// fired. The trigger fires when `consecutive_not_achieved >=
    /// last_strategist_fired_at + N`, which is SKIP-ROBUST: the synthetic
    /// concurrent-in-flight path can bump the streak past a multiple of N
    /// without landing exactly on it, and a strict `% N == 0` check would
    /// then miss the fire. Reset to 0 with `consecutive_not_achieved`.
    #[serde(default)]
    pub last_strategist_fired_at: u32,
    /// Added to the resolved classifier cap once the strategist has fired.
    #[serde(default)]
    pub strategist_cap_bonus: u32,

    /// Path to the most recent strategist strategy note on disk
    /// (`<session_dir>/goal/strategy.md`, via
    /// [`GoalTracker::strategy_path`]). `None` until the strategist runs.
    /// Surfaced in the continuation directive so the model re-reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_strategy_path: Option<String>,
    /// Short narrative recommendation read back from the strategist's
    /// note (capped). Inlined into the continuation directive until a
    /// later strategist run overwrites it; cleared on an `Achieved`
    /// verdict (same replay convention as `last_classifier_gaps`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_strategy_recommendation: Option<String>,

    /// `git rev-parse HEAD` captured at goal creation. Used by the
    /// classifier to diff the worktree against the goal's baseline.
    /// `None` for goals created before the baseline-capture wiring
    /// landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes_baseline_commit: Option<String>,

    /// Path to the goal's plan markdown (`<session_dir>/goal/plan.md`,
    /// via [`GoalTracker::plan_path`]). `None` until a planner writes
    /// one. `is_some()` is the single source of truth for "this goal
    /// has a plan" — gates setup-time fire, the resume-retry path,
    /// and the load-time reconciler. Persisted across restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_file: Option<PathBuf>,

    /// Path to the immutable snapshot of the planner's ORIGINAL plan
    /// (`<session_dir>/goal/plan.baseline.md`, via
    /// [`GoalTracker::plan_baseline_path`]). Captured once right after the
    /// planner first writes `plan_file`; never overwritten on later attempts
    /// or restarts. The verifier diffs the CURRENT plan against it
    /// (`capture_plan_changes`) so a skeptic sees every edit the agent made to
    /// `plan.md` during the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_baseline_file: Option<PathBuf>,

    /// True once the harness created and squat-verified the scratch root AND
    /// the implementer subdir, so prompts can honestly say the dir exists.
    /// `#[serde(skip)]`: recomputed by `from_snapshot` on every reload (the
    /// sole reload path), so a persisted value would be dead-on-read — same as
    /// the recomputed/transient `live_*` fields below.
    #[serde(skip)]
    pub scratch_dir_ready: bool,

    // Transient live-progress fields (not persisted)
    #[serde(skip)]
    pub live_subagent_tokens: u64,
    /// Per-model marginal-token breakdown (model_id, tokens), sorted by
    /// tokens descending. Transient mirror of the active goal's subagent
    /// token records; `#[serde(skip)]` so legacy snapshots deserialize and
    /// it is never persisted.
    #[serde(skip)]
    pub live_tokens_by_model: Vec<(String, u64)>,
    #[serde(skip)]
    pub live_context_window: u64,
    #[serde(skip)]
    pub live_context_pct: u8,
    #[serde(skip)]
    pub live_turn_count: u32,
    #[serde(skip)]
    pub live_tool_call_count: u32,

    /// True while the goal planner subagent is running. Latched by
    /// `emit_goal_planning` and reset after the planner finishes so the
    /// "planning…" badge survives the subagent-spawn / token-accounting
    /// `GoalUpdated`s that fire mid-run. Transient (never persisted).
    #[serde(skip)]
    pub planning_in_flight: bool,

    /// True while the verification skeptic panel is running. Latched around
    /// the verification stage (mirrors `planning_in_flight`) so the
    /// "Verifying…" badge survives the token-accounting / continuation
    /// `GoalUpdated`s that fire mid-verification. Transient (never persisted).
    #[serde(skip)]
    pub verifying_in_flight: bool,
}

impl GoalOrchestration {
    /// Reset ALL strategist state in one place: the consecutive-NotAchieved
    /// streak, the last-fired marker, and the persisted recommendation +
    /// path. Coupling these means a streak reset can never leave a stale
    /// structural recommendation replaying into a clean run (a past
    /// reset/clear asymmetry across branches). Called wherever the goal
    /// starts a fresh streak (`Achieved`/`Blocked` verdicts) or ends/resumes
    /// (`complete`/`budget_limit`/`resume`).
    fn reset_strategist_fields(&mut self) {
        self.consecutive_not_achieved = 0;
        self.last_strategist_fired_at = 0;
        self.strategist_cap_bonus = 0;
        self.last_strategy_path = None;
        self.last_strategy_recommendation = None;
    }

    /// Clear the gap-fingerprint stall streak (shared reset so every caller
    /// zeroes the same fields).
    fn reset_classifier_stall_fields(&mut self) {
        self.last_gap_fingerprint = None;
        self.classifier_stall_count = 0;
    }

    fn reset_evaluator_blocker_fields(&mut self) {
        self.evaluator_blocker_key = None;
        self.evaluator_blocked_streak = 0;
    }
}

// GoalTracker (pure state machine)

#[derive(Debug)]
pub struct GoalTracker {
    orchestration: Option<GoalOrchestration>,
    session_dir: PathBuf,
    active_since: Option<Instant>,
    planner_run: Option<GoalPlannerRunState>,
}

#[derive(Debug)]
pub(crate) struct GoalPlannerRunState {
    pub(crate) cancel: tokio_util::sync::CancellationToken,
    pub(crate) steering: Vec<String>,
}

impl GoalTracker {
    pub fn new(session_dir: PathBuf) -> Self {
        Self {
            orchestration: None,
            session_dir,
            active_since: None,
            planner_run: None,
        }
    }

    /// Restore from a persisted snapshot.
    ///
    /// If the snapshot had an in-flight phase (Planning, Executing),
    /// reset to Idle because subagents don't survive a restart. An
    /// in-flight `Active` goal becomes `UserPaused`; other paused
    /// variants (including [`GoalStatus::InfraPaused`]) are preserved
    /// so `pause_message` and pause cause stay aligned. Clear
    /// `current_subagent_id`.
    pub(crate) fn from_snapshot(session_dir: PathBuf, mut snapshot: GoalOrchestration) -> Self {
        if matches!(snapshot.phase, GoalPhase::Planning | GoalPhase::Executing) {
            snapshot.phase = GoalPhase::Idle;
            snapshot.current_subagent_id = None;
            snapshot.current_subagent_role = None;
        }
        if snapshot.status == GoalStatus::Active {
            snapshot.status = GoalStatus::UserPaused;
        }
        // `planning_in_flight` / `verifying_in_flight` are `#[serde(skip)]` but
        // in-memory-clone callers bypass that; reset explicitly.
        snapshot.planning_in_flight = false;
        snapshot.verifying_in_flight = false;
        // Token records anchoring a resumed skeptic-0's marginal accounting
        // are in-memory only; a post-restart resume would re-count its full
        // prior cumulative as fresh spend. Cold-spawn instead.
        snapshot.skeptic0_session_id = None;
        // `verifier_id` is snapshot-controlled and embedded in paths later
        // fed to `remove_dir_all`; a non-canonical id (e.g. `/../`) could
        // escape the temp root. Enforce the pinned 12-hex form.
        if snapshot.verifier_id.len() != 12
            || !snapshot.verifier_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            snapshot.verifier_id = generate_verifier_id();
        }
        // Best-effort like `create_goal`: scratch rarely survives a restart
        // but the skeptics expect it. Terminal snapshots had theirs removed
        // deliberately — don't resurrect.
        if !matches!(
            snapshot.status,
            GoalStatus::Complete | GoalStatus::BudgetLimited
        ) {
            // Subdirs only under a verified root — a squatted root must
            // not receive writes through `create_dir_all`. Recompute readiness
            // so a resumed prompt only claims the dir exists when it does.
            snapshot.scratch_dir_ready = match ensure_goal_scratch_root(&snapshot.verifier_id) {
                Ok(_) => {
                    std::fs::create_dir_all(implementer_scratch_dir(&snapshot.verifier_id)).is_ok()
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "goal restore: could not secure the scratch root; skipping scratch creation",
                    );
                    false
                }
            };
        } else {
            // The prompt must not claim a dir the terminal transition removed.
            snapshot.scratch_dir_ready = false;
        }
        let active_since = if snapshot.status == GoalStatus::Active {
            Some(Instant::now())
        } else {
            None
        };
        Self {
            orchestration: Some(snapshot),
            session_dir,
            active_since,
            planner_run: None,
        }
    }

    pub fn snapshot(&self) -> Option<&GoalOrchestration> {
        self.orchestration.as_ref()
    }

    pub(crate) fn start_planner_run(&mut self, cancel: tokio_util::sync::CancellationToken) {
        self.planner_run = Some(GoalPlannerRunState {
            cancel,
            steering: Vec::new(),
        });
    }

    pub(crate) fn take_planner_run(&mut self) -> Option<GoalPlannerRunState> {
        self.planner_run.take()
    }

    pub(crate) fn steer_planner(&mut self, steering: String) {
        let Some(run) = self.planner_run.as_mut() else {
            return;
        };
        if steering.is_empty() {
            return;
        }
        run.steering.push(steering);
        run.cancel.cancel();
    }

    pub(crate) fn snapshot_mut(&mut self) -> Option<&mut GoalOrchestration> {
        self.orchestration.as_mut()
    }

    pub fn is_active(&self) -> bool {
        self.orchestration
            .as_ref()
            .is_some_and(|o| o.status == GoalStatus::Active)
    }

    pub fn phase(&self) -> Option<GoalPhase> {
        self.orchestration.as_ref().map(|o| o.phase)
    }

    pub fn status(&self) -> Option<GoalStatus> {
        self.orchestration.as_ref().map(|o| o.status)
    }

    pub fn current_subagent_id(&self) -> Option<&str> {
        self.orchestration
            .as_ref()
            .and_then(|o| o.current_subagent_id.as_deref())
    }

    pub fn objective(&self) -> Option<&str> {
        self.orchestration.as_ref().map(|o| o.objective.as_str())
    }

    pub fn token_budget(&self) -> Option<i64> {
        self.orchestration.as_ref().and_then(|o| o.token_budget)
    }

    fn goal_dir(&self) -> PathBuf {
        self.session_dir.join("goal")
    }

    /// Path to the goal's plan markdown (`<session_dir>/goal/plan.md`);
    /// may not exist yet.
    pub fn plan_path(&self) -> PathBuf {
        self.goal_dir().join("plan.md")
    }

    /// Path to the immutable baseline snapshot of the planner's original
    /// plan (`<session_dir>/goal/plan.baseline.md`); written once after
    /// the planner first produces `plan.md`. Sibling of [`Self::plan_path`].
    pub(crate) fn plan_baseline_path(&self) -> PathBuf {
        self.goal_dir().join("plan.baseline.md")
    }

    /// Path to the strategist's advisory note (`<session_dir>/goal/strategy.md`).
    /// The strategist writes here, NOT `plan.md`. Its `PlanGuard` snapshots
    /// `plan.md` and restores it byte-for-byte (and on cancellation),
    /// reverting ANY strategist edit. Whole-file restore is safe because the
    /// strategist runs synchronously as the sole writer (the goal turn is
    /// blocked awaiting it), so there's no concurrent implementer edit to
    /// clobber. Sibling of [`Self::plan_path`]; may not exist until the
    /// strategist first runs.
    pub(crate) fn strategy_path(&self) -> PathBuf {
        self.goal_dir().join("strategy.md")
    }

    /// Move the last classifier details file out of the scratch root
    /// (which the caller is about to remove) into the durable session
    /// goal dir and update `last_classifier_details_path`, so the
    /// "See <path>" surfaced by the achieved ack stays readable;
    /// per-skeptic reports are inlined below it
    /// ([`append_skeptic_reports`]) to keep it self-contained.
    ///
    /// The stored source path is snapshot-controlled: `..` components
    /// are rejected (`starts_with` is lexical), the source root must
    /// pass [`verify_owned_real_dir`] (a squatted root could stage an
    /// attacker file for the move), and the copy fallback is
    /// [`copy_no_follow`]. The path is stamped only after a move from
    /// the verified root succeeds; otherwise it is left unchanged.
    ///
    /// Runs under the tracker lock — bounded I/O (≤ the panel cap) on
    /// cold, one-shot transitions.
    fn rescue_classifier_details(&mut self) {
        let goal_dir = self.goal_dir();
        let Some(o) = self.orchestration.as_mut() else {
            return;
        };
        let Some(src) = o.last_classifier_details_path.as_deref() else {
            return;
        };
        let src = PathBuf::from(src);
        if src
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return;
        }
        // Only artifacts inside THIS goal's scratch root are at risk.
        let scratch_root = goal_scratch_root(&o.verifier_id);
        if !src.starts_with(&scratch_root) {
            return;
        }
        // A symlink-squatted root could stage an attacker file for the
        // rename below.
        if verify_owned_real_dir(&scratch_root).is_err() {
            return;
        }
        let Some(name) = src.file_name() else {
            return;
        };
        let dest = goal_dir.join(name);
        let _ = std::fs::create_dir_all(&goal_dir);
        if std::fs::rename(&src, &dest).is_ok() || copy_no_follow(&src, &dest).is_ok() {
            append_skeptic_reports(&scratch_root, &dest);
            o.last_classifier_details_path = Some(dest.to_string_lossy().into_owned());
        }
    }

    /// Create a new goal. Replaces any existing orchestration.
    ///
    /// `baseline_commit` is the `git rev-parse HEAD` captured at the
    /// call site (or `None` if not available). It is consumed by
    /// value — callers with `&str` should `.to_string()` at the call
    /// site rather than have the helper clone.
    pub(crate) fn create_goal(
        &mut self,
        goal_id: String,
        objective: String,
        token_budget: Option<i64>,
        token_baseline: i64,
        created_at: String,
        baseline_commit: Option<String>,
    ) {
        let _ = std::fs::create_dir_all(self.goal_dir());
        // Replacing a still-active goal: same rescue-then-remove contract
        // as the terminal transitions (the prior goal's details path may
        // already be in user-visible messages).
        if self.orchestration.is_some() {
            self.rescue_classifier_details();
            self.remove_scratch_root();
        }
        // Private per-goal scratch: the implementer dir is created up
        // front (the goal model writes throwaway artifacts here from its
        // first round); each `skeptic-<idx>` dir is created lazily when
        // that skeptic spawns. Best-effort — a creation failure degrades
        // to the model's own fallback, never blocks goal setup.
        let verifier_id = generate_verifier_id();
        // Subdirs only under a verified root (see `ensure_goal_scratch_root`).
        // Capture whether the implementer dir is truly on disk so the prompts
        // only claim "created for you" when it is.
        let scratch_dir_ready = match ensure_goal_scratch_root(&verifier_id) {
            Ok(_) => std::fs::create_dir_all(implementer_scratch_dir(&verifier_id)).is_ok(),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "goal create: could not secure the scratch root; skipping scratch creation",
                );
                false
            }
        };
        self.orchestration = Some(GoalOrchestration {
            goal_id,
            objective,
            status: GoalStatus::Active,
            phase: GoalPhase::Executing,
            token_budget,
            elapsed_ms: 0,
            created_at,
            current_subagent_id: None,
            current_subagent_role: None,
            total_worker_rounds: 0,
            total_verify_rounds: 0,
            budget_limit_reported: false,
            token_baseline,
            tokens_used_high_water: 0,
            parent_tokens_spent: 0,
            last_session_tokens_seen: Some(token_baseline),
            history: Vec::new(),
            pause_message: None,
            evaluator_blocker_key: None,
            evaluator_blocked_streak: 0,
            verifier_id,
            classifier_runs_attempted: 0,
            rounds_since_verify: 0,
            classifier_max_runs: None,
            last_classifier_verdict: None,
            last_classifier_details_path: None,
            last_classifier_at: None,
            last_classifier_gaps: None,
            first_final_response: None,
            skeptic0_session_id: None,
            skeptic_model_assignment: Vec::new(),
            last_gap_fingerprint: None,
            classifier_stall_count: 0,
            consecutive_not_achieved: 0,
            last_strategist_fired_at: 0,
            strategist_cap_bonus: 0,
            last_strategy_path: None,
            last_strategy_recommendation: None,
            changes_baseline_commit: baseline_commit,
            plan_file: None,
            plan_baseline_file: None,
            scratch_dir_ready,
            live_subagent_tokens: 0,
            live_tokens_by_model: Vec::new(),
            live_context_window: 0,
            live_context_pct: 0,
            live_turn_count: 0,
            live_tool_call_count: 0,
            planning_in_flight: false,
            verifying_in_flight: false,
        });
        self.active_since = Some(Instant::now());
        self.record_event(GoalEvent::GoalCreated, None);
    }

    pub fn set_phase(&mut self, phase: GoalPhase) {
        if let Some(o) = &mut self.orchestration {
            o.phase = phase;
        }
    }

    pub fn set_current_subagent(&mut self, id: Option<String>, role: Option<String>) {
        if let Some(o) = &mut self.orchestration {
            o.current_subagent_id = id;
            o.current_subagent_role = role;
        }
    }

    /// Pause the goal with a specific reason. Only transitions from `Active`.
    /// Flushes elapsed time and stops the timer. The `reason` selects one
    /// of the paused variants of [`GoalStatus`]. The `pause_message` field
    /// on the orchestration is NOT modified — to stash a human-readable
    /// reason alongside the pause, use [`Self::pause_with_message`].
    /// Returns `true` if the transition was applied.
    pub fn pause(&mut self, reason: GoalPauseReason) -> bool {
        self.pause_inner(reason, None)
    }

    /// Like [`Self::pause`] but also stores a human-readable `message` on
    /// [`GoalOrchestration::pause_message`]. Used by the `Verification`
    /// reason so the user-visible block reason survives until the next
    /// transition out of the paused state.
    /// Returns `true` if the transition was applied.
    pub(crate) fn pause_with_message(&mut self, reason: GoalPauseReason, message: String) -> bool {
        self.pause_inner(reason, Some(message))
    }

    fn pause_inner(&mut self, reason: GoalPauseReason, message: Option<String>) -> bool {
        let applied = if let Some(o) = &mut self.orchestration
            && o.status == GoalStatus::Active
        {
            if let Some(since) = self.active_since.take() {
                o.elapsed_ms = o
                    .elapsed_ms
                    .saturating_add(since.elapsed().as_millis() as u64);
            }
            o.status = reason.to_status();
            if message.is_some() {
                o.pause_message = message;
            }
            true
        } else {
            false
        };
        if applied {
            self.record_event(
                GoalEvent::GoalPaused,
                Some(reason.history_detail().to_owned()),
            );
        }
        applied
    }

    /// Resume a paused goal (any paused variant, including `Blocked`). Sets
    /// `active_since`, clears [`GoalOrchestration::pause_message`], and resets
    /// every per-attempt auto-pause counter — `classifier_runs_attempted`, the
    /// strategist streak/recommendation, and the gap-fingerprint stall streak —
    /// so a user re-arm starts fully fresh. Returns `true` if applied.
    pub fn resume(&mut self) -> bool {
        if let Some(o) = &mut self.orchestration
            && o.status.is_paused()
        {
            o.status = GoalStatus::Active;
            o.pause_message = None;
            o.classifier_runs_attempted = 0;
            o.rounds_since_verify = 0;
            o.reset_strategist_fields();
            o.reset_classifier_stall_fields();
            o.reset_evaluator_blocker_fields();
            self.active_since = Some(Instant::now());
            self.record_event(GoalEvent::GoalResumed, None);
            return true;
        }
        false
    }

    /// Mark the goal as complete. Accepts `Active` or any paused variant.
    /// Returns `true` if the transition was applied.
    pub fn complete(&mut self) -> bool {
        if let Some(o) = &mut self.orchestration
            && (o.status == GoalStatus::Active || o.status.is_paused())
        {
            if let Some(since) = self.active_since.take() {
                o.elapsed_ms = o
                    .elapsed_ms
                    .saturating_add(since.elapsed().as_millis() as u64);
            }
            o.status = GoalStatus::Complete;
            o.phase = GoalPhase::Idle;
            o.current_subagent_id = None;
            o.current_subagent_role = None;
            o.pause_message = None;
            // Drop the resumed reject-gatekeeper so any later goal starts
            // verification with a fresh, cold skeptic 0.
            o.skeptic0_session_id = None;
            // Sibling of skeptic 0: drop the frozen per-index model
            // assignment so a later goal re-resolves its own panel.
            o.skeptic_model_assignment.clear();
            // Drop the plan baseline alongside skeptic 0: a later goal
            // re-snapshots its own planner's original plan.
            o.plan_baseline_file = None;
            // Terminal transition: reset all strategist state so a
            // recreated/reactivated goal never inherits a stale count or note.
            o.reset_strategist_fields();
            o.reset_evaluator_blocker_fields();
            // The achieved ack points the user at the details file, so it
            // must outlive the scratch-root removal below.
            self.rescue_classifier_details();
            self.remove_scratch_root();
            self.record_event(GoalEvent::GoalCompleted, None);
            return true;
        }
        false
    }

    /// Mark the goal as budget-limited. Accepts `Active` or any paused variant.
    /// Returns `true` if the transition was applied.
    pub(crate) fn budget_limit(&mut self) -> bool {
        if let Some(o) = &mut self.orchestration
            && (o.status == GoalStatus::Active || o.status.is_paused())
        {
            if let Some(since) = self.active_since.take() {
                o.elapsed_ms = o
                    .elapsed_ms
                    .saturating_add(since.elapsed().as_millis() as u64);
            }
            o.status = GoalStatus::BudgetLimited;
            o.phase = GoalPhase::Idle;
            o.current_subagent_id = None;
            o.current_subagent_role = None;
            o.pause_message = None;
            // Symmetric with `complete`: drop the resumed reject-gatekeeper,
            // the frozen per-index model assignment, and the plan baseline on
            // every terminal goal-ending transition.
            o.skeptic0_session_id = None;
            o.skeptic_model_assignment.clear();
            o.plan_baseline_file = None;
            o.reset_strategist_fields();
            o.reset_evaluator_blocker_fields();
            // Symmetric with `complete`.
            self.rescue_classifier_details();
            self.remove_scratch_root();
            self.record_event(GoalEvent::BudgetExceeded, None);
            return true;
        }
        false
    }

    /// Clear the goal entirely (`GoalClear`). Dropping the whole
    /// orchestration also drops `plan_baseline_file` / `skeptic0_session_id`,
    /// so no per-field reset is needed here — but the on-disk scratch root
    /// outlives the struct, so rescue the surfaced details file and remove
    /// the root explicitly first (mirrors the `complete` / `budget_limit`
    /// cleanup).
    pub fn clear(&mut self) {
        self.rescue_classifier_details();
        self.remove_scratch_root();
        self.orchestration = None;
        self.active_since = None;
    }

    /// Best-effort scratch-root removal shared by every terminal
    /// transition; a distinct `verifier_id` per goal means this never
    /// touches a concurrent goal's dir.
    fn remove_scratch_root(&self) {
        if let Some(o) = &self.orchestration {
            let _ = std::fs::remove_dir_all(goal_scratch_root(&o.verifier_id));
        }
    }

    /// Overwrite the transient live-display fields from one subagent's
    /// progress tick. Single-slot, last-writer-wins by design: a display
    /// hint may flip between concurrent children; authoritative totals
    /// come from the token records.
    pub(crate) fn update_live_progress(
        &mut self,
        subagent_tokens: u64,
        tokens_by_model: Vec<(String, u64)>,
        context_window: u64,
        context_pct: u8,
        turn_count: u32,
        tool_call_count: u32,
    ) {
        if let Some(o) = &mut self.orchestration {
            o.live_subagent_tokens = subagent_tokens;
            o.live_tokens_by_model = tokens_by_model;
            o.live_context_window = context_window;
            o.live_context_pct = context_pct;
            o.live_turn_count = turn_count;
            o.live_tool_call_count = tool_call_count;
        }
    }

    /// Flush elapsed wall-clock time into `elapsed_ms`.
    pub(crate) fn account_elapsed(&mut self) {
        if let Some(o) = &mut self.orchestration
            && let Some(since) = self.active_since
        {
            o.elapsed_ms = o
                .elapsed_ms
                .saturating_add(since.elapsed().as_millis() as u64);
            self.active_since = Some(Instant::now());
        }
    }

    /// Record a `NotAchieved` rejection's gap `fingerprint` and report
    /// whether the goal has stalled — i.e. the same fingerprint has now
    /// appeared on enough consecutive rejections that the model changed
    /// nothing in the flagged gaps. The threshold is
    /// [`GOAL_CLASSIFIER_STALL_THRESHOLD`], relaxed to
    /// [`GOAL_STRATEGIST_STALL_THRESHOLD`] while a strategist restructure is
    /// in flight (cap bonus active). A fingerprint that differs from the
    /// previous one resets the streak to its first occurrence. No-op
    /// (returns `false`) without an orchestration.
    pub(crate) fn record_classifier_stall(&mut self, fingerprint: &str) -> bool {
        let Some(o) = self.orchestration.as_mut() else {
            return false;
        };
        if o.last_gap_fingerprint.as_deref() == Some(fingerprint) {
            o.classifier_stall_count = o.classifier_stall_count.saturating_add(1);
        } else {
            o.last_gap_fingerprint = Some(fingerprint.to_string());
            o.classifier_stall_count = 1;
        }
        let threshold = if o.strategist_cap_bonus > 0 {
            GOAL_STRATEGIST_STALL_THRESHOLD
        } else {
            GOAL_CLASSIFIER_STALL_THRESHOLD
        };
        o.classifier_stall_count >= threshold
    }

    pub(crate) fn record_evaluator_blocker(&mut self, blocker_key: &str) -> u32 {
        let Some(o) = self.orchestration.as_mut() else {
            return 0;
        };
        if o.evaluator_blocker_key.as_deref() == Some(blocker_key) {
            o.evaluator_blocked_streak = o.evaluator_blocked_streak.saturating_add(1);
        } else {
            o.evaluator_blocker_key = Some(blocker_key.to_owned());
            o.evaluator_blocked_streak = 1;
        }
        o.evaluator_blocked_streak
    }

    pub(crate) fn reset_evaluator_blocker(&mut self) {
        if let Some(o) = self.orchestration.as_mut() {
            o.reset_evaluator_blocker_fields();
        }
    }

    /// Undo the most recent attempt-slot reservation. Used when a
    /// rejection is routed to the `Blocked` outcome so it does not
    /// consume the retry budget the user gets back on resume.
    pub(crate) fn rollback_classifier_attempt(&mut self) {
        if let Some(o) = self.orchestration.as_mut() {
            o.classifier_runs_attempted = o.classifier_runs_attempted.saturating_sub(1);
        }
    }

    /// Clear the stall streak so the next rejection starts a fresh
    /// fingerprint comparison. Used on the `Blocked` route — a paused-for-
    /// user goal must not carry a half-built streak into its resume.
    pub(crate) fn reset_classifier_stall(&mut self) {
        if let Some(o) = self.orchestration.as_mut() {
            o.reset_classifier_stall_fields();
        }
    }

    /// Increment the consecutive-`NotAchieved` streak and return the new
    /// value. Drives the (skip-robust) strategist trigger. No-op (returns
    /// 0) without an orchestration.
    pub(crate) fn record_not_achieved_streak(&mut self) -> u32 {
        match self.orchestration.as_mut() {
            Some(o) => {
                o.consecutive_not_achieved = o.consecutive_not_achieved.saturating_add(1);
                o.consecutive_not_achieved
            }
            None => 0,
        }
    }

    /// Atomically evaluate the strategist trigger and claim a fire under a
    /// single lock: if `should_fire(consecutive_not_achieved,
    /// last_strategist_fired_at)` holds, mark the fire at the current streak
    /// (so the next fire needs another N failures) and return
    /// `Some(consecutive_not_achieved)`; otherwise leave state untouched and
    /// return `None`. Folding the check and the record into one critical
    /// section keeps them indivisible, so a streak can never double-fire the
    /// strategist. On fire it also grants the cap bonus and resets the
    /// gap-fingerprint stall streak so the restructure runs against a relaxed,
    /// freshly-measured stall window. No-op (`None`) without an orchestration.
    pub(crate) fn claim_strategist_fire(
        &mut self,
        should_fire: impl Fn(u32, u32) -> bool,
    ) -> Option<u32> {
        let o = self.orchestration.as_mut()?;
        if should_fire(o.consecutive_not_achieved, o.last_strategist_fired_at) {
            o.last_strategist_fired_at = o.consecutive_not_achieved;
            o.strategist_cap_bonus = GOAL_STRATEGIST_CAP_BONUS; // idempotent, not stacked
            o.reset_classifier_stall_fields();
            Some(o.consecutive_not_achieved)
        } else {
            None
        }
    }

    /// Revoke the cap bonus granted by [`Self::claim_strategist_fire`] when
    /// the strategist delivered no restructure. `last_strategist_fired_at`
    /// keeps the claim so the next fire still waits a full window.
    /// Deliberately conservative: the bonus is set, never stacked, so this
    /// also wipes an earlier successful fire's bonus — capping early beats
    /// running unearned rounds under a relaxed stall guard.
    pub(crate) fn revoke_strategist_cap_bonus(&mut self) {
        if let Some(o) = self.orchestration.as_mut() {
            o.strategist_cap_bonus = 0;
        }
    }

    /// Reset ALL strategist state (streak + last-fired marker + persisted
    /// recommendation). Called on an `Achieved` verdict (streak broken) and
    /// the `Blocked` route (paused for the user). Symmetric with the
    /// `complete`/`budget_limit`/`resume` resets so a paused/solved goal
    /// never replays a stale recommendation.
    pub(crate) fn reset_strategist_state(&mut self) {
        if let Some(o) = self.orchestration.as_mut() {
            o.reset_strategist_fields();
        }
    }

    /// Persist the strategist's latest output path + short recommendation
    /// so the continuation directive can inline them. No-op without an
    /// orchestration.
    pub(crate) fn record_strategy_recommendation(&mut self, path: String, recommendation: String) {
        if let Some(o) = self.orchestration.as_mut() {
            o.last_strategy_path = Some(path);
            o.last_strategy_recommendation = Some(recommendation);
        }
    }

    pub(crate) fn append_history(&mut self, entry: GoalHistoryEntry) {
        if let Some(o) = &mut self.orchestration {
            o.history.push(entry);
            // Bound the persisted timeline; drop oldest past the cap.
            let overflow = o.history.len().saturating_sub(GOAL_HISTORY_MAX);
            if overflow > 0 {
                o.history.drain(0..overflow);
            }
        }
    }

    /// Append a timestamped lifecycle history entry. The single chokepoint for
    /// every transition (create / pause / resume / complete / budget_limit) so
    /// each reaches `GoalUpdated.last_event` and no branch can forget to record.
    fn record_event(&mut self, event: GoalEvent, detail: Option<String>) {
        self.append_history(GoalHistoryEntry::now(event, detail));
    }
}

// Test helpers (shared across goal_tracker + goal_orchestrator tests)

#[cfg(test)]
pub(crate) fn make_base_orchestration() -> GoalOrchestration {
    GoalOrchestration {
        goal_id: "g-test".into(),
        objective: "test objective".into(),
        status: GoalStatus::Active,
        phase: GoalPhase::Idle,
        token_budget: None,
        elapsed_ms: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        current_subagent_id: None,
        current_subagent_role: None,
        total_worker_rounds: 0,
        total_verify_rounds: 0,
        budget_limit_reported: false,
        token_baseline: 0,
        tokens_used_high_water: 0,
        parent_tokens_spent: 0,
        last_session_tokens_seen: Some(0),
        history: Vec::new(),
        pause_message: None,
        evaluator_blocker_key: None,
        evaluator_blocked_streak: 0,
        verifier_id: generate_verifier_id(),
        classifier_runs_attempted: 0,
        rounds_since_verify: 0,
        classifier_max_runs: None,
        last_classifier_verdict: None,
        last_classifier_details_path: None,
        last_classifier_at: None,
        last_classifier_gaps: None,
        first_final_response: None,
        skeptic0_session_id: None,
        skeptic_model_assignment: Vec::new(),
        last_gap_fingerprint: None,
        classifier_stall_count: 0,
        consecutive_not_achieved: 0,
        last_strategist_fired_at: 0,
        strategist_cap_bonus: 0,
        last_strategy_path: None,
        last_strategy_recommendation: None,
        changes_baseline_commit: None,
        plan_file: None,
        plan_baseline_file: None,
        scratch_dir_ready: false,
        live_subagent_tokens: 0,
        live_tokens_by_model: Vec::new(),
        live_context_window: 0,
        live_context_pct: 0,
        live_turn_count: 0,
        live_tool_call_count: 0,
        planning_in_flight: false,
        verifying_in_flight: false,
    }
}

// Tests

#[cfg(test)]
#[path = "goal_tracker_tests.rs"]
mod tests;
