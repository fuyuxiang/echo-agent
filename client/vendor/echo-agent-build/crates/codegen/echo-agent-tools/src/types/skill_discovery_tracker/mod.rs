//! Session-scoped skill lifecycle manager.
//!
//! `SkillManager` owns the full skill lifecycle: startup baseline, dynamic
//! discoveries, combined projections, pending updates, and announcement
//! dedup. The session actor never stores skill state; it triggers state
//! transitions via the `ToolBridge` and executes the resulting
//! `SkillUpdateEffects`.

mod conditional;
mod listing;
mod skill_path_suggestion;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::implementations::skills::types::SkillInfo;
use crate::types::compat::CompatConfig;

use conditional::ConditionalSkills;
use listing::{DEFAULT_SKILL_TOOL_NAME, SKILL_BUDGET_CONTEXT_PERCENT, format_announcement};

pub use listing::{XmlRenderMode, format_announcement_xml, format_compaction_skill_listing};

/// Why a `SkillUpdateEffects` was produced.
///
/// Lets the session distinguish dynamic discoveries (the model navigated
/// into a directory containing a new `SKILL.md`) from baseline changes
/// (session start, plugin reload, `/clear`). Some templates suppress one
/// but not the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillUpdateKind {
    /// New skills discovered dynamically while the session is running
    /// (path-driven discovery from a tool call). Default because it is
    /// the safe choice if a future caller forgets to set this.
    #[default]
    Discovery,
    /// Startup baseline established (or replaced via plugin reload /
    /// `/clear`).
    BaselineChange,
}

/// The model-facing skill listing as rendered by
/// [`SkillManager::listing_snapshot`], used for `/context` accounting.
#[derive(Debug, Clone)]
pub struct SkillListingSnapshot {
    /// The rendered listing, including the XML envelope in XML mode.
    pub text: String,
    /// Number of skills that qualified for the listing. Under extreme
    /// budget pressure the names-only tier can drop trailing entries from
    /// `text` while they remain counted here.
    pub skill_count: usize,
}

/// Conversation/UI side-effects the session must perform after a skill update.
///
/// The tools layer handles all skill-domain logic (projections, dedup,
/// writing `AvailableSkills`). This struct carries only the effects that
/// require session capabilities: injecting a `<system-reminder>` message
/// and refreshing slash command advertisement. Slash command data is read
/// from `bridge.slash_skills()`, not from this struct.
#[derive(Debug, Clone, Default)]
pub struct SkillUpdateEffects {
    /// If `Some`, inject this text as a `<system-reminder>` user message.
    /// Used for both dynamic discovery announcements and baseline change
    /// notifications. The system prompt is never mutated for skills.
    pub system_reminder: Option<String>,
    /// If true, send updated slash commands to the client.
    /// The session reads the skill list from `bridge.slash_skills()`.
    pub send_available_commands: bool,
    /// Why this update was produced. Lets harnesses suppress one kind
    /// without suppressing the other — see [`SkillUpdateKind`].
    pub kind: SkillUpdateKind,
}

/// Authoritative state for the full skill lifecycle.
///
/// Owns both the startup baseline and dynamic discoveries. The session
/// never stores skill state; it triggers state transitions here and
/// executes the resulting `SkillUpdateEffects`.
///
/// Named `SkillManager` (not `SkillDiscoveryTracker`) because it manages
/// the full lifecycle: startup baseline, dynamic discovery, projections,
/// announcements, compaction, and /clear.
#[derive(Debug, Clone, Default)]
pub struct SkillManager {
    /// Skills loaded at session start (or refreshed on plugin reload).
    /// This is the non-dynamic baseline.
    startup_skills: Vec<SkillInfo>,

    /// A desktop automation turn supplies its immutable revision in-band.
    /// Hide the mutable catalog entry of that same skill for the turn.
    suppressed_skill: Option<String>,

    /// Directories already stat'd (prevents re-checking on future tool calls).
    pub checked_dirs: HashSet<PathBuf>,

    /// Real cwd path prefix to rewrite for model-visible display.
    /// When set (forked sessions), announcement formatting replaces
    /// this prefix with `display_cwd` so the model never sees overlay paths.
    real_cwd_prefix: Option<String>,
    /// Display cwd to substitute for `real_cwd_prefix` in announcements.
    display_cwd: Option<String>,

    /// Skills discovered during this session via file-tool triggers.
    discovered_skills: Vec<SkillInfo>,

    /// Canonical paths of discovered skills (for fast dedup on insert).
    discovered_canonical_paths: HashSet<PathBuf>,

    /// Skill names already announced via system-reminder (prevents
    /// duplicate reminder text).
    announced_names: HashSet<String>,

    /// Git root for upward path walking (canonicalized).
    pub git_root: Option<PathBuf>,

    /// Current working directory (canonicalized).
    pub cwd: Option<PathBuf>,

    /// Pending reconciliation. Set by `add_discovered()` or baseline
    /// changes. Drained by `take_pending_reconciliation()`.
    pending: Option<PendingKind>,

    /// Chat budget for skill listing system-reminders.
    /// Falls back to `DEFAULT_CHAR_BUDGET` when `None`.
    listing_budget_chars: Option<usize>,

    /// Client-facing name of the skill tool (resolved from `TemplateRenderer`).
    /// Falls back to `"Skill"` when not set.
    skill_tool_name: Option<String>,

    /// Client-facing name of the read tool (resolved from `TemplateRenderer`).
    /// Falls back to `"Read"` when not set. Used in the `<available_skills>`
    /// description attribute of mid-session XML skill announcements.
    read_tool_name: Option<String>,

    /// When true, `take_pending()` formats announcements as XML instead of
    /// markdown.
    use_xml_format: bool,

    /// Resolved vendor-compat config governing which vendor dirs dynamic
    /// skill discovery scans. Defaults to all-on (historical behavior).
    /// Set by the bridge at seed time; read by `SkillDiscoveryReminder`.
    pub(crate) compat: CompatConfig,

    /// `paths:`-gated skills held back from the listing until a matching file
    /// is touched, plus their activation state. See [`ConditionalSkills`].
    conditional: ConditionalSkills,

    /// Every skill name from session-start discovery, set once by
    /// `ToolRegistryBuilder::finalize` from the unfiltered
    /// `SessionContext.skills` and never rewritten by later seeds or
    /// baseline reloads. The `paths:` gate never applies here.
    discovery_snapshot_names: Vec<String>,
}

/// Canonicalize a skill path, falling back to the raw path for not-yet-created
/// files or symlink-resolution failures.
fn canonical_path(path: &str) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// Why a reconciliation is pending.
#[derive(Debug, Clone)]
enum PendingKind {
    /// New skills discovered (dynamic). Announcement needed.
    Discovery,
    /// Baseline changed (plugin reload / /clear). Prompt re-render needed.
    BaselineChange,
}

/// Dedup helpers -- defined here so the tracker can compute projections
/// without depending on echo-agent-core.
fn dedup_by_canonical_path(primary: &[SkillInfo], secondary: &[SkillInfo]) -> Vec<SkillInfo> {
    let mut seen_paths = HashSet::new();
    let mut result = Vec::with_capacity(primary.len() + secondary.len());
    for skill in primary.iter().chain(secondary.iter()) {
        let canonical =
            dunce::canonicalize(&skill.path).unwrap_or_else(|_| PathBuf::from(&skill.path));
        if seen_paths.insert(canonical) {
            result.push(skill.clone());
        }
    }
    result
}

fn dedupe_by_canonical_path_and_name(
    primary: &[SkillInfo],
    secondary: &[SkillInfo],
) -> Vec<SkillInfo> {
    let mut seen_paths = HashSet::new();
    let mut seen_names = HashSet::new();
    let mut result = Vec::with_capacity(primary.len() + secondary.len());
    for skill in primary.iter().chain(secondary.iter()) {
        let canonical =
            dunce::canonicalize(&skill.path).unwrap_or_else(|_| PathBuf::from(&skill.path));
        if !seen_paths.insert(canonical) {
            continue;
        }
        if !seen_names.insert(skill.dedup_key()) {
            continue;
        }
        result.push(skill.clone());
    }
    result
}

/// Inputs for [`render_listing`].
struct ListingRenderParams<'a> {
    real_prefix: Option<&'a str>,
    display_prefix: Option<&'a str>,
    budget: Option<usize>,
    use_xml_format: bool,
    read_tool_name: &'a str,
    skill_tool_name: &'a str,
}

/// Render the model-facing listing for `skills`, deduplicating against
/// `announced`. Keys are inserted before budget truncation, so `announced`
/// tracks skills that qualified, not skills whose text survived.
///
/// Both [`SkillManager::take_pending`] and
/// [`SkillManager::listing_snapshot`] render through this path, so the
/// injected reminders and the `/context` estimate cannot drift apart.
fn render_listing(
    skills: &[SkillInfo],
    announced: &mut HashSet<String>,
    params: &ListingRenderParams<'_>,
) -> Option<String> {
    if params.use_xml_format {
        // Same envelope as the startup preamble, so startup and
        // mid-session listings share one structure.
        let read_tool = params.read_tool_name;
        let envelope_open = format!(
            "<agent_skills>\n\
             <available_skills description=\"Newly discovered skills. \
             Use the {read_tool} tool with the provided absolute path to fetch full contents.\">\n"
        );
        let envelope_close = "</available_skills>\n</agent_skills>";
        format_announcement_xml(
            skills,
            announced,
            params.real_prefix,
            params.display_prefix,
            // The XML format is only enabled by templates that request it.
            XmlRenderMode::Verbatim,
        )
        .map(|rows| format!("{envelope_open}{rows}{envelope_close}"))
    } else {
        format_announcement(
            skills,
            announced,
            params.real_prefix,
            params.display_prefix,
            params.budget,
            params.skill_tool_name,
        )
    }
}

impl SkillManager {
    /// Create a new tracker with no state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the client-facing name of the skill tool.
    pub fn set_skill_tool_name(&mut self, name: String) {
        self.skill_tool_name = Some(name);
    }

    /// Set the client-facing name of the read tool.
    pub fn set_read_tool_name(&mut self, name: String) {
        self.read_tool_name = Some(name);
    }

    /// Enable XML formatting for skill announcements.
    pub fn set_xml_format(&mut self, enabled: bool) {
        self.use_xml_format = enabled;
    }

    /// Set the resolved vendor-compat config used by dynamic skill discovery.
    /// Must be called at session start (alongside `seed`) so the
    /// `SkillDiscoveryReminder` gates vendor dirs correctly.
    pub fn set_compat(&mut self, compat: CompatConfig) {
        self.compat = compat;
    }

    /// Pre-populate `announced_names` from persisted state.
    ///
    /// Must be called BEFORE `seed()`.  When `announced_names` is non-empty,
    /// `seed()` skips setting `pending = BaselineChange`, preventing
    /// duplicate skill listing injection on session resume.
    pub fn restore_announced_names(&mut self, names: HashSet<String>) {
        self.announced_names = names;
    }

    /// Get the current announced names set (for persistence).
    pub fn announced_names(&self) -> &HashSet<String> {
        &self.announced_names
    }

    /// Seed the tracker with session context and startup skills.
    ///
    /// Called once at session start. Sets the git root, cwd, and the
    /// initial startup baseline. Marks a pending baseline change so
    /// the first `apply_pending_skill_update()` delivers the startup
    /// skill listing as a `<system-reminder>`.
    ///
    /// When `announced_names` is already non-empty (restored from persisted
    /// state), the pending is NOT set — the conversation history already
    /// contains the skill listing from the previous session.
    ///
    /// `display_cwd`: If set (forked sessions), the real cwd prefix in
    /// skill paths is replaced with this value in model-visible announcements.
    /// Runtime invocation always uses the real path.
    pub fn seed(
        &mut self,
        cwd: Option<PathBuf>,
        git_root: Option<PathBuf>,
        startup_skills: Vec<SkillInfo>,
        display_cwd: Option<String>,
        context_window_tokens: Option<u64>,
        skill_budget_percent: Option<f64>,
    ) {
        let percent = skill_budget_percent.unwrap_or(SKILL_BUDGET_CONTEXT_PERCENT);
        self.listing_budget_chars =
            context_window_tokens.map(|tokens| (tokens as f64 * 4.0 * percent) as usize);
        // Store the real cwd as string for path prefix rewriting.
        if let Some(ref display) = display_cwd {
            if let Some(ref c) = cwd {
                self.real_cwd_prefix = Some(c.to_string_lossy().to_string());
            }
            self.display_cwd = Some(display.clone());
        }
        self.cwd = cwd.map(|p| dunce::canonicalize(&p).unwrap_or(p));
        self.git_root = git_root.map(|p| dunce::canonicalize(&p).unwrap_or(p));
        let unconditional = self.conditional.take_unconditional(startup_skills);
        let has_skills = !unconditional.is_empty();
        self.startup_skills = unconditional;
        // Only set pending if announced_names is empty (fresh session).
        // When announced_names is non-empty (restored from persistence),
        // the model's conversation history already contains the skill
        // listing from the previous session — no re-announcement needed.
        if has_skills && self.announced_names.is_empty() {
            self.pending = Some(PendingKind::BaselineChange);
        }
    }

    /// Replace the startup baseline (plugin reload / bundle sync).
    ///
    /// Marks a pending baseline-change reconciliation only if the skill set
    /// actually changed (by canonical path). This prevents duplicate
    /// `<system-reminder>` injections when a bundle sync completes with the
    /// same skills that were already seeded at startup.
    ///
    /// Dynamic discoveries are preserved.
    pub fn update_startup_baseline(&mut self, new_skills: Vec<SkillInfo>) {
        let old_paths: HashSet<String> =
            self.startup_skills.iter().map(|s| s.path.clone()).collect();
        let unconditional = self.conditional.take_unconditional(new_skills);
        let new_paths: HashSet<String> = unconditional.iter().map(|s| s.path.clone()).collect();
        let changed = old_paths != new_paths;
        self.startup_skills = unconditional;
        if changed {
            self.pending = Some(PendingKind::BaselineChange);
        }
    }

    /// Add newly discovered skills. Returns true if any entered the listing.
    ///
    /// Deduplication is by canonical filesystem path. `paths:`-gated skills are
    /// held back (same gate as `seed`) so dynamic discovery can't leak them into
    /// the listing before a matching file is touched.
    pub fn add_discovered(&mut self, skills: Vec<SkillInfo>) -> bool {
        let mut any_new = false;
        for skill in skills {
            let canonical = canonical_path(&skill.path);
            if self.discovered_canonical_paths.contains(&canonical) {
                continue;
            }
            if self.conditional.is_pending(&skill) {
                self.conditional.hold_dynamic(skill);
                continue;
            }
            self.discovered_canonical_paths.insert(canonical);
            self.discovered_skills.push(skill);
            any_new = true;
        }
        if any_new {
            self.pending = Some(PendingKind::Discovery);
        }
        any_new
    }

    /// Activate conditional skills whose `paths:` match any touched file, then
    /// surface them via `add_discovered`. Returns true if any activated.
    pub fn activate_conditional_skills_for_paths(&mut self, touched: &[&std::path::Path]) -> bool {
        if self.conditional.is_empty() {
            return false;
        }
        let Some(cwd) = self.cwd.clone() else {
            return false;
        };
        let newly = self.conditional.activate_for_paths(touched, &cwd);
        if newly.is_empty() {
            return false;
        }
        self.add_discovered(newly)
    }

    pub(crate) fn suppress_skill(&mut self, name: Option<String>) -> bool {
        if self.suppressed_skill == name {
            return false;
        }
        self.suppressed_skill = name;
        self.pending = Some(PendingKind::BaselineChange);
        true
    }

    pub(crate) fn is_suppressed(&self, name: &str) -> bool {
        self.suppressed_skill.as_deref() == Some(name)
    }

    pub(crate) fn is_suppressed_path(&self, path: &std::path::Path) -> bool {
        let Some(name) = self.suppressed_skill.as_deref() else { return false; };
        let home = crate::util::echo_agent_home::echo_agent_home();
        let library = home.join("workflow-library");
        let library = dunce::canonicalize(&library).unwrap_or(library);
        path.starts_with(library) && path.parent().and_then(|parent| parent.file_name())
            .is_some_and(|directory| directory == name)
    }

    /// Drain the pending reconciliation, computing projections internally
    /// and returning only the runtime skills and session side-effects.
    ///
    /// Returns `(runtime_skills, effects)` if there is a pending change,
    /// or `None` if nothing changed.
    ///
    /// `runtime_skills` must be written into `AvailableSkills` by the
    /// caller (the bridge's `apply_pending_skill_update` method).
    ///
    /// `effects` contains only conversation/UI side-effects the session
    /// must perform.
    pub fn take_pending(&mut self) -> Option<(Vec<SkillInfo>, SkillUpdateEffects)> {
        let pending = self.pending.take()?;

        let mut runtime_skills = dedup_by_canonical_path(&self.discovered_skills, &self.startup_skills);
        runtime_skills.retain(|skill| !self.is_suppressed(&skill.name));

        let kind = match pending {
            PendingKind::Discovery => SkillUpdateKind::Discovery,
            PendingKind::BaselineChange => SkillUpdateKind::BaselineChange,
        };
        // Take the announced set out of `self` so `render_listing` can
        // borrow `self` immutably alongside it; restored below.
        let mut announced = std::mem::take(&mut self.announced_names);
        let skills_owned = match pending {
            PendingKind::Discovery => None,
            PendingKind::BaselineChange => {
                announced.clear();
                Some(dedupe_by_canonical_path_and_name(
                    &self.discovered_skills,
                    &self.startup_skills,
                ))
            }
        };
        let skills = skills_owned.as_deref().unwrap_or(&self.discovered_skills);
        let filtered;
        let skills = if self.suppressed_skill.is_some() {
            filtered = skills.iter()
                .filter(|skill| !self.is_suppressed(&skill.name))
                .cloned()
                .collect::<Vec<_>>();
            &filtered
        } else {
            skills
        };
        let system_reminder = render_listing(skills, &mut announced, &self.render_params());
        self.announced_names = announced;

        // Disabled skills are omitted from the listing via `s.enabled` in
        // `format_announcement` / `format_announcement_xml`. Do not append a
        // separate "must not be used" name footer — it wastes tokens and
        // looks like skills with no description.

        let effects = SkillUpdateEffects {
            system_reminder,
            send_available_commands: true,
            kind,
        };

        Some((runtime_skills, effects))
    }

    /// Public test-only accessor for verifying full reconciliation results.
    #[cfg(test)]
    pub fn take_pending_reconciliation(&mut self) -> Option<TestReconciliation> {
        let (runtime_skills, effects) = self.take_pending()?;
        Some(TestReconciliation {
            runtime_skills,
            effects,
        })
    }

    /// Get the display-deduped skill list for slash commands.
    ///
    /// Combines startup + discovered, deduplicates by canonical path
    /// and name (discovered wins). This is the authoritative source
    /// for slash command advertisement.
    pub fn slash_skills(&self) -> Vec<SkillInfo> {
        let mut skills = dedupe_by_canonical_path_and_name(&self.discovered_skills, &self.startup_skills);
        skills.retain(|skill| !self.is_suppressed(&skill.name));
        skills
    }

    /// Set the full-discovery snapshot (see `discovery_snapshot_names`).
    /// Write-once by design: the only caller is `ToolRegistryBuilder::finalize`.
    pub(crate) fn set_discovery_snapshot_names(&mut self, names: Vec<String>) {
        self.discovery_snapshot_names = names;
    }

    /// Every skill name from session-start discovery. Set only by
    /// `ToolRegistryBuilder::finalize`; empty for a manager seeded
    /// without it.
    pub fn discovery_snapshot_names(&self) -> &[String] {
        &self.discovery_snapshot_names
    }

    /// Render the canonical listing for the entire current skill set, for
    /// `/context` accounting. Leaves announce state untouched.
    ///
    /// The result estimates what the listing costs in context; it is not a
    /// replay of the injected reminders, which accumulate incrementally.
    /// Returns `None` when no skill qualifies.
    pub fn listing_snapshot(&self) -> Option<SkillListingSnapshot> {
        let skills = self.slash_skills();
        // A throwaway set keeps `self.announced_names` untouched and, once
        // filled by the renderer's filter pass, counts the qualifying skills.
        let mut qualified = HashSet::new();
        let text = render_listing(&skills, &mut qualified, &self.render_params())?;
        Some(SkillListingSnapshot {
            text,
            skill_count: qualified.len(),
        })
    }

    /// Listing-render inputs shared by the reminder and snapshot paths.
    fn render_params(&self) -> ListingRenderParams<'_> {
        ListingRenderParams {
            real_prefix: self.real_cwd_prefix.as_deref(),
            display_prefix: self.display_cwd.as_deref(),
            budget: self.listing_budget_chars,
            use_xml_format: self.use_xml_format,
            read_tool_name: self.read_tool_name.as_deref().unwrap_or("Read"),
            skill_tool_name: self
                .skill_tool_name
                .as_deref()
                .unwrap_or(DEFAULT_SKILL_TOOL_NAME),
        }
    }

    /// Get the current dynamically discovered skills.
    pub fn discovered_skills(&self) -> &[SkillInfo] {
        &self.discovered_skills
    }

    /// Get the current startup skills baseline.
    pub fn startup_skills(&self) -> &[SkillInfo] {
        &self.startup_skills
    }

    /// Reset discovery state for compaction.
    ///
    /// Clears `announced_names` so the reminder will re-announce on
    /// the next file access after compaction, and clears `checked_dirs`
    /// so dynamically discovered skills can be re-discovered if the
    /// model navigates back into the same directories.
    ///
    /// Does NOT clear `discovered_skills` (those are preserved for the
    /// compaction context and slash commands).
    pub fn on_compaction(&mut self) {
        self.announced_names.clear();
        self.checked_dirs.clear();
    }

    /// Full reset for `/clear`.
    ///
    /// Clears all discovery state. Startup baseline is preserved.
    /// Marks a pending baseline-change reconciliation so surfaces
    /// get rebuilt from baseline only.
    pub fn on_clear(&mut self) {
        self.discovered_skills.clear();
        self.discovered_canonical_paths.clear();
        self.checked_dirs.clear();
        self.announced_names.clear();
        // Re-hide every conditional skill on /clear.
        let startup = std::mem::take(&mut self.startup_skills);
        self.startup_skills = self.conditional.rehide(startup);
        self.pending = Some(PendingKind::BaselineChange);
    }
}

/// Test-only reconciliation result with full projection visibility.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct TestReconciliation {
    pub runtime_skills: Vec<SkillInfo>,
    pub effects: SkillUpdateEffects,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_skill(name: &str, path: &str) -> SkillInfo {
        SkillInfo {
            name: name.to_owned(),
            description: format!("desc for {name}"),
            path: path.to_owned(),
            ..SkillInfo::default()
        }
    }

    fn make_conditional_skill(name: &str, path: &str, patterns: &[&str]) -> SkillInfo {
        SkillInfo {
            paths: Some(patterns.iter().map(|p| p.to_string()).collect()),
            ..make_skill(name, path)
        }
    }

    #[test]
    fn pinned_skill_stays_hidden_until_the_next_ordinary_turn() {
        let mut manager = SkillManager::new();
        manager.seed(None, None, vec![
            make_skill("echo-workflow-preview", "/original/SKILL.md"),
            make_skill("ordinary", "/ordinary/SKILL.md"),
        ], None, None, None);
        assert!(manager.suppress_skill(Some("echo-workflow-preview".into())));
        let first = manager.take_pending_reconciliation().unwrap();
        assert_eq!(first.runtime_skills.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(), ["ordinary"]);
        assert!(!first.effects.system_reminder.unwrap_or_default().contains("echo-workflow-preview"));
        manager.update_startup_baseline(vec![make_skill("echo-workflow-preview", "/updated/SKILL.md")]);
        assert!(manager.take_pending_reconciliation().unwrap().runtime_skills.is_empty());
        assert!(manager.slash_skills().is_empty());
        assert!(manager.suppress_skill(None));
        assert_eq!(manager.take_pending_reconciliation().unwrap().runtime_skills[0].path, "/updated/SKILL.md");
    }

    #[test]
    fn conditional_skill_hidden_until_match() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(
            Some(cwd.clone()),
            None,
            vec![
                make_skill("always", "/a/SKILL.md"),
                make_conditional_skill("gated", "/g/SKILL.md", &["src/{main,lib}.rs"]),
            ],
            None,
            None,
            None,
        );

        // Baseline listing omits the gated skill.
        let r = mgr.take_pending_reconciliation().unwrap();
        assert!(r.runtime_skills.iter().any(|s| s.name == "always"));
        assert!(!r.runtime_skills.iter().any(|s| s.name == "gated"));

        // Touching a matching file activates it.
        let touched = cwd.join("src").join("main.rs");
        assert!(mgr.activate_conditional_skills_for_paths(&[touched.as_path()]));
        let r = mgr.take_pending_reconciliation().unwrap();
        assert!(r.runtime_skills.iter().any(|s| s.name == "gated"));
    }

    #[test]
    fn dynamically_discovered_conditional_skill_is_held_back() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(Some(cwd.clone()), None, vec![], None, None, None);

        // Dynamic discovery (reminder path) must apply the `paths:` gate too.
        let any_new = mgr.add_discovered(vec![make_conditional_skill(
            "gated",
            "/g/SKILL.md",
            &["src/**"],
        )]);
        assert!(
            !any_new,
            "gated skill must not enter the listing on discovery"
        );
        assert!(mgr.discovered_skills().is_empty());

        let touched = cwd.join("src").join("main.rs");
        assert!(mgr.activate_conditional_skills_for_paths(&[touched.as_path()]));
        assert_eq!(mgr.discovered_skills().len(), 1);
    }

    #[test]
    fn dynamic_conditional_survives_baseline_reseed() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(Some(cwd.clone()), None, vec![], None, None, None);

        mgr.add_discovered(vec![make_conditional_skill(
            "gated",
            "/g/SKILL.md",
            &["src/**"],
        )]);
        // A reload whose new baseline doesn't list the dynamic skill.
        mgr.update_startup_baseline(vec![make_skill("always", "/a/SKILL.md")]);

        // The held skill survives and still activates on a matching touch.
        let touched = cwd.join("src").join("main.rs");
        assert!(mgr.activate_conditional_skills_for_paths(&[touched.as_path()]));
        assert!(mgr.discovered_skills().iter().any(|s| s.name == "gated"));
    }

    #[test]
    fn conditional_non_match_stays_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(
            Some(cwd.clone()),
            None,
            vec![make_conditional_skill("gated", "/g/SKILL.md", &["src/**"])],
            None,
            None,
            None,
        );
        let _ = mgr.take_pending_reconciliation();

        let touched = cwd.join("docs").join("readme.md");
        assert!(!mgr.activate_conditional_skills_for_paths(&[touched.as_path()]));
        assert!(mgr.take_pending_reconciliation().is_none());
    }

    #[test]
    fn activated_conditional_survives_reseed() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(
            Some(cwd.clone()),
            None,
            vec![make_conditional_skill("gated", "/g/SKILL.md", &["src/**"])],
            None,
            None,
            None,
        );
        let _ = mgr.take_pending_reconciliation();
        let touched = cwd.join("src").join("lib.rs");
        assert!(mgr.activate_conditional_skills_for_paths(&[touched.as_path()]));
        let _ = mgr.take_pending_reconciliation();

        // Re-seed (e.g. plugin reload): the activated skill stays visible.
        mgr.seed(
            Some(cwd.clone()),
            None,
            vec![make_conditional_skill("gated", "/g/SKILL.md", &["src/**"])],
            None,
            None,
            None,
        );
        let slash = mgr.slash_skills();
        assert!(slash.iter().any(|s| s.name == "gated"));
    }

    #[test]
    fn reload_holds_back_conditional_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(Some(cwd), None, vec![], None, None, None);
        let _ = mgr.take_pending_reconciliation();

        mgr.update_startup_baseline(vec![
            make_skill("always", "/a/SKILL.md"),
            make_conditional_skill("gated", "/g/SKILL.md", &["src/**"]),
        ]);
        let r = mgr.take_pending_reconciliation().unwrap();
        assert!(r.runtime_skills.iter().any(|s| s.name == "always"));
        assert!(!r.runtime_skills.iter().any(|s| s.name == "gated"));
    }

    #[test]
    fn on_clear_rehides_reload_promoted_conditional() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(tmp.path()).unwrap();
        let mut mgr = SkillManager::new();
        mgr.seed(
            Some(cwd.clone()),
            None,
            vec![make_conditional_skill("gated", "/g/SKILL.md", &["src/**"])],
            None,
            None,
            None,
        );
        let _ = mgr.take_pending_reconciliation();

        // Activate, then reload (promotes the activated skill into the baseline).
        let touched = cwd.join("src").join("main.rs");
        assert!(mgr.activate_conditional_skills_for_paths(&[touched.as_path()]));
        let _ = mgr.take_pending_reconciliation();
        mgr.update_startup_baseline(vec![make_conditional_skill(
            "gated",
            "/g/SKILL.md",
            &["src/**"],
        )]);
        let _ = mgr.take_pending_reconciliation();

        // /clear must re-hide it (not leave it stuck in the baseline).
        mgr.on_clear();
        let r = mgr.take_pending_reconciliation().unwrap();
        assert!(!r.runtime_skills.iter().any(|s| s.name == "gated"));
    }

    #[test]
    fn add_discovered_deduplicates_by_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("alpha");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "---\nname: alpha\n---\n").unwrap();
        let path = skill_dir.join("SKILL.md").to_str().unwrap().to_string();

        let mut tracker = SkillManager::new();
        assert!(tracker.add_discovered(vec![make_skill("alpha", &path)]));
        assert!(!tracker.add_discovered(vec![make_skill("alpha", &path)]));
        assert_eq!(tracker.discovered_skills().len(), 1);
    }

    #[test]
    fn pending_set_only_on_new_skills() {
        let mut tracker = SkillManager::new();
        assert!(tracker.take_pending_reconciliation().is_none());

        tracker.add_discovered(vec![make_skill("alpha", "/a/SKILL.md")]);
        assert!(tracker.take_pending_reconciliation().is_some());

        // Same canonical path: no new pending.
        tracker.add_discovered(vec![make_skill("alpha", "/a/SKILL.md")]);
        assert!(tracker.take_pending_reconciliation().is_none());

        // New path: new pending.
        tracker.add_discovered(vec![make_skill("beta", "/b/SKILL.md")]);
        assert!(tracker.take_pending_reconciliation().is_some());
    }

    #[test]
    fn same_name_different_paths_coexist_in_runtime() {
        let mut tracker = SkillManager::new();
        tracker.seed(None, None, vec![], None, None, None);
        tracker.add_discovered(vec![
            make_skill("deploy", "/path/a/deploy/SKILL.md"),
            make_skill("deploy", "/path/b/deploy/SKILL.md"),
        ]);
        let r = tracker.take_pending_reconciliation().unwrap();
        assert_eq!(r.runtime_skills.len(), 2);
    }

    #[test]
    fn seed_stores_startup_and_context() {
        let mut tracker = SkillManager::new();
        let tmp = tempfile::tempdir().unwrap();
        tracker.seed(
            Some(tmp.path().to_path_buf()),
            Some(tmp.path().to_path_buf()),
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        assert!(tracker.cwd.is_some());
        assert!(tracker.git_root.is_some());
        assert_eq!(tracker.startup_skills().len(), 1);
    }

    #[test]
    fn on_clear_removes_discovered_preserves_startup() {
        let mut tracker = SkillManager::new();
        tracker.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        tracker.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = tracker.take_pending_reconciliation(); // drain

        tracker.on_clear();
        assert!(tracker.discovered_skills().is_empty());
        assert_eq!(tracker.startup_skills().len(), 1);

        // Clear queues a baseline-change reconciliation with system-reminder.
        let r = tracker.take_pending_reconciliation().unwrap();
        assert!(
            r.effects.system_reminder.is_some(),
            "baseline change should produce system-reminder"
        );
        assert_eq!(r.runtime_skills.len(), 1);
        assert_eq!(r.runtime_skills[0].name, "startup");
    }

    #[test]
    fn on_clear_preserves_session_context() {
        let mut tracker = SkillManager::new();
        let tmp = tempfile::tempdir().unwrap();
        tracker.seed(
            Some(tmp.path().to_path_buf()),
            Some(tmp.path().to_path_buf()),
            vec![],
            None,
            None,
            None,
        );
        tracker.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);

        tracker.on_clear();
        assert!(tracker.cwd.is_some());
        assert!(tracker.git_root.is_some());
    }

    #[test]
    fn compaction_preserves_discovered_skills() {
        let mut tracker = SkillManager::new();
        tracker.add_discovered(vec![make_skill("alpha", "/a/SKILL.md")]);
        let _ = tracker.take_pending_reconciliation(); // drain

        tracker.on_compaction();
        assert_eq!(tracker.discovered_skills().len(), 1);
    }

    /// Pin that `slash_skills()` (used by compaction) returns both startup
    /// and discovered skills after compaction. This is the core invariant
    /// that ensures startup/baseline skills survive compaction.
    #[test]
    fn compaction_snapshot_includes_startup_and_discovered_skills() {
        let mut tracker = SkillManager::new();
        tracker.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        tracker.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = tracker.take_pending_reconciliation(); // drain

        tracker.on_compaction();

        // slash_skills() is what the compaction call site uses
        let snapshot = tracker.slash_skills();
        assert!(
            snapshot.iter().any(|s| s.name == "startup"),
            "startup skills must appear in compaction snapshot"
        );
        assert!(
            snapshot.iter().any(|s| s.name == "dyn"),
            "discovered skills must appear in compaction snapshot"
        );
    }

    #[test]
    fn compaction_clears_checked_dirs() {
        let mut tracker = SkillManager::new();
        tracker.checked_dirs.insert(PathBuf::from("/some/dir"));
        tracker.checked_dirs.insert(PathBuf::from("/other/dir"));
        assert_eq!(tracker.checked_dirs.len(), 2);

        tracker.on_compaction();
        assert!(
            tracker.checked_dirs.is_empty(),
            "checked_dirs must be cleared on compaction so re-discovery works"
        );
    }

    /// Re-discovery of the same skill after compaction must not produce
    /// duplicates. `checked_dirs` is cleared (so the dir is re-scanned),
    /// but `discovered_canonical_paths` is preserved (so `add_discovered`
    /// deduplicates and returns `false`).
    #[test]
    fn rediscovery_after_compaction_does_not_duplicate() {
        let mut tracker = SkillManager::new();
        tracker.checked_dirs.insert(PathBuf::from("/d"));
        tracker.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = tracker.take_pending_reconciliation(); // drain

        tracker.on_compaction();
        assert!(tracker.checked_dirs.is_empty(), "checked_dirs cleared");

        // Re-discover the same skill (same canonical path)
        let is_new = tracker.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        assert!(!is_new, "same canonical path must not be treated as new");
        assert_eq!(
            tracker.discovered_skills().len(),
            1,
            "must not duplicate discovered skills"
        );
    }

    #[test]
    fn update_startup_baseline_queues_baseline_change() {
        let mut tracker = SkillManager::new();
        tracker.seed(
            None,
            None,
            vec![make_skill("old", "/old/SKILL.md")],
            None,
            None,
            None,
        );
        let _ = tracker.take_pending_reconciliation(); // drain startup

        tracker.update_startup_baseline(vec![make_skill("new", "/new/SKILL.md")]);
        let r = tracker.take_pending_reconciliation().unwrap();
        assert!(
            r.effects.system_reminder.is_some(),
            "baseline change should produce system-reminder"
        );
        // Slash skills are read from the manager directly, not from effects.
        let slash = tracker.slash_skills();
        assert_eq!(slash.len(), 1);
        assert_eq!(slash[0].name, "new");
    }

    /// When the startup baseline is replaced with the same set of skill
    /// paths, no pending reconciliation should be queued.  This prevents
    /// duplicate `<system-reminder>` injections when a bundle sync completes
    /// with an unchanged skill set.
    #[test]
    fn update_startup_baseline_same_paths_skips_pending() {
        let mut tracker = SkillManager::new();
        tracker.seed(
            None,
            None,
            vec![make_skill("s1", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let _ = tracker.take_pending_reconciliation(); // drain startup

        // Replace with the exact same path — should NOT queue a pending.
        tracker.update_startup_baseline(vec![make_skill("s1", "/s/SKILL.md")]);
        assert!(
            tracker.take_pending_reconciliation().is_none(),
            "same paths must not produce a duplicate system-reminder"
        );
    }

    #[test]
    fn plugin_reload_preserves_discovered() {
        let mut tracker = SkillManager::new();
        tracker.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        tracker.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = tracker.take_pending_reconciliation(); // drain discovery

        tracker.update_startup_baseline(vec![make_skill("new-startup", "/ns/SKILL.md")]);
        let r = tracker.take_pending_reconciliation().unwrap();
        // Runtime should have both new-startup and dyn.
        assert_eq!(r.runtime_skills.len(), 2);
        assert!(r.runtime_skills.iter().any(|s| s.name == "new-startup"));
        assert!(r.runtime_skills.iter().any(|s| s.name == "dyn"));
    }

    #[test]
    fn add_discovered_after_clear_works() {
        let mut tracker = SkillManager::new();
        tracker.seed(None, None, vec![], None, None, None);
        tracker.add_discovered(vec![make_skill("old", "/old/SKILL.md")]);
        let _ = tracker.take_pending_reconciliation();

        tracker.on_clear();
        let _ = tracker.take_pending_reconciliation(); // drain clear

        tracker.add_discovered(vec![make_skill("new", "/new/SKILL.md")]);
        assert!(tracker.take_pending_reconciliation().is_some());
        assert_eq!(tracker.discovered_skills().len(), 1);
        assert_eq!(tracker.discovered_skills()[0].name, "new");
    }

    // ── Architecture invariant tests ──────────────────────────────

    #[test]
    fn seed_produces_system_reminder_not_prompt_mutation() {
        // Invariant: startup skills are delivered via system-reminder,
        // never via system prompt mutation.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        // Must produce a system-reminder for the model to see skills.
        assert!(r.effects.system_reminder.is_some());
        let text = r.effects.system_reminder.unwrap();
        assert!(text.contains("startup"));
        // Must NOT contain <system-reminder> tags (shell adds those).
        assert!(!text.contains("<system-reminder>"));
    }

    #[test]
    fn no_skills_seed_produces_no_pending() {
        // Empty startup should not produce a pending update.
        let mut mgr = SkillManager::new();
        mgr.seed(None, None, vec![], None, None, None);
        assert!(mgr.take_pending_reconciliation().is_none());
    }

    #[test]
    fn slash_skills_returns_current_combined_set() {
        // slash_skills() is the authoritative source for slash commands.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let _ = mgr.take_pending_reconciliation();

        assert_eq!(mgr.slash_skills().len(), 1);
        assert_eq!(mgr.slash_skills()[0].name, "startup");

        mgr.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation();

        assert_eq!(mgr.slash_skills().len(), 2);
    }

    #[test]
    fn clear_then_slash_skills_returns_baseline_only() {
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        mgr.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation();

        mgr.on_clear();
        let _ = mgr.take_pending_reconciliation();

        let slash = mgr.slash_skills();
        assert_eq!(slash.len(), 1);
        assert_eq!(slash[0].name, "startup");
    }

    #[test]
    fn plugin_reload_then_slash_skills_includes_discovered() {
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("old-startup", "/os/SKILL.md")],
            None,
            None,
            None,
        );
        mgr.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation();

        mgr.update_startup_baseline(vec![make_skill("new-startup", "/ns/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation();

        let slash = mgr.slash_skills();
        assert_eq!(slash.len(), 2);
        assert!(slash.iter().any(|s| s.name == "new-startup"));
        assert!(slash.iter().any(|s| s.name == "dyn"));
    }

    #[test]
    fn compaction_does_not_produce_pending() {
        // Compaction clears announced_names but does NOT queue a pending.
        // Re-announcement happens when the reminder re-fires after compaction.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("s", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        mgr.add_discovered(vec![make_skill("d", "/d/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation();

        mgr.on_compaction();
        assert!(mgr.take_pending_reconciliation().is_none());
        // But discovered skills are preserved for the compaction context.
        assert_eq!(mgr.discovered_skills().len(), 1);
    }

    #[test]
    fn effects_never_contains_skill_data() {
        // SkillUpdateEffects must not carry skill-domain payloads. The
        // only fields are session-side knobs: a rendered reminder text,
        // a bool for slash command refresh, and a kind discriminator
        // so harnesses can suppress one update kind without the other.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("s", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        let effects = r.effects;
        let _ = effects.system_reminder;
        let _ = effects.send_available_commands;
        let _ = effects.kind;
        // If this test compiles, SkillUpdateEffects has no extra fields
        // leaking skill-domain data into the shell.
    }

    /// Pin the kind discriminator so harnesses that suppress one
    /// reminder kind (e.g. a harness suppressing `BaselineChange` because
    /// the preamble already snapshots the baseline) do not accidentally
    /// suppress the other.
    #[test]
    fn effects_kind_distinguishes_baseline_from_discovery() {
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/startup/SKILL.md")],
            None,
            None,
            None,
        );
        let baseline = mgr.take_pending_reconciliation().unwrap();
        assert_eq!(baseline.effects.kind, SkillUpdateKind::BaselineChange);

        mgr.add_discovered(vec![make_skill("found", "/found/SKILL.md")]);
        let discovery = mgr.take_pending_reconciliation().unwrap();
        assert_eq!(discovery.effects.kind, SkillUpdateKind::Discovery);
    }

    // ── Display-path rewriting tests ─────────────────────────────

    #[test]
    fn display_cwd_rewrites_announcement_paths() {
        let mut mgr = SkillManager::new();
        mgr.seed(
            Some("/overlay/worktree".into()),
            None,
            vec![make_skill(
                "deploy",
                "/overlay/worktree/.echo-agent/skills/deploy/SKILL.md",
            )],
            Some("/home/user/project".to_string()),
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        // Display path must use the display cwd.
        assert!(text.contains("/home/user/project"));
        // Real overlay path must NOT appear in announcement.
        assert!(!text.contains("/overlay/worktree"));
    }

    #[test]
    fn no_display_cwd_shows_real_paths() {
        let mut mgr = SkillManager::new();
        mgr.seed(
            Some("/real/path".into()),
            None,
            vec![make_skill(
                "deploy",
                "/real/path/.echo-agent/skills/deploy/SKILL.md",
            )],
            None,
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(text.contains("/real/path"));
    }

    // ── /clear + startup visibility tests ─────────────────────────

    #[test]
    fn on_clear_then_drain_produces_startup_listing() {
        // After /clear, draining the pending must produce a system-reminder
        // with the startup baseline skills. This proves /clear followed by
        // apply_pending_skill_update() gives the model visibility.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let _ = mgr.take_pending_reconciliation(); // drain initial

        mgr.add_discovered(vec![make_skill("dyn", "/d/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation(); // drain discovery

        mgr.on_clear();
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        // Startup skill must be re-announced after /clear.
        assert!(text.contains("startup"));
        // Dynamic skill must NOT appear after /clear.
        assert!(!text.contains("dyn"));
    }

    #[test]
    fn on_clear_idempotent_on_empty_tracker() {
        // Fresh tracker: on_clear is a no-op on empty state,
        // but still queues a baseline-change pending.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("s", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let _ = mgr.take_pending_reconciliation(); // drain seed

        mgr.on_clear();
        let r = mgr.take_pending_reconciliation().unwrap();
        // Must still produce a listing of the baseline.
        assert!(r.effects.system_reminder.is_some());
    }

    // ── Budget-cap tests ────────────────────────────────────────

    #[test]
    fn budget_cap_truncates_long_descriptions() {
        let mut mgr = SkillManager::new();
        let skills: Vec<SkillInfo> = (0..50)
            .map(|i| {
                let mut s = make_skill(&format!("skill-{i}"), &format!("/s/{i}/SKILL.md"));
                s.description = "A".repeat(300); // 300 chars each, well over budget
                s
            })
            .collect();
        // 128k context window → budget = 128_000 * 4 * 0.5 = 256000 chars
        let context_window: u64 = 128_000;
        let expected_budget = (context_window as f64 * 4.0 * SKILL_BUDGET_CONTEXT_PERCENT) as usize;
        mgr.seed(None, None, skills, None, Some(context_window), None);
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(
            text.len() <= expected_budget + 100,
            "listing should be near budget ({expected_budget}), got {} chars",
            text.len()
        );
    }

    #[test]
    fn budget_cap_names_only_when_extreme() {
        let mut mgr = SkillManager::new();
        let skills: Vec<SkillInfo> = (0..200)
            .map(|i| {
                let mut s = make_skill(&format!("skill-{i}"), &format!("/s/{i}/SKILL.md"));
                s.description = "A".repeat(500);
                s
            })
            .collect();
        // 300 token context window → budget = 300 * 4 * 0.5 = 600 chars.
        // 200 skills with 500-char descriptions can't fit.
        let context_window: u64 = 300;
        let expected_budget = (context_window as f64 * 4.0 * SKILL_BUDGET_CONTEXT_PERCENT) as usize;
        mgr.seed(None, None, skills, None, Some(context_window), None);
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(
            text.len() <= expected_budget + 100,
            "listing should be near budget ({expected_budget}), got {} chars",
            text.len()
        );
        assert!(text.contains("... and"), "should indicate truncated skills");
    }

    #[test]
    fn budget_cap_no_truncation_within_budget() {
        let mut mgr = SkillManager::new();
        let skills = vec![
            make_skill("commit", "/s/commit/SKILL.md"),
            make_skill("review", "/s/review/SKILL.md"),
        ];
        // 200k context window → 12000 char budget, 2 skills fit easily.
        mgr.seed(None, None, skills, None, Some(200_000), None);
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(text.contains("desc for commit"));
        assert!(text.contains("desc for review"));
    }

    // ── XML format mode tests ───────────────────────────────────

    #[test]
    fn xml_format_produces_agent_skill_tags_with_envelope() {
        let mut mgr = SkillManager::new();
        mgr.set_xml_format(true);
        mgr.seed(
            None,
            None,
            vec![make_skill("commit", "/s/commit/SKILL.md")],
            None,
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(
            text.contains("<agent_skills>"),
            "XML format should wrap in <agent_skills> envelope: {text}"
        );
        assert!(
            text.contains("<available_skills"),
            "XML format should include <available_skills> wrapper: {text}"
        );
        assert!(
            text.contains("<agent_skill fullPath="),
            "XML format should produce <agent_skill> tags: {text}"
        );
        assert!(
            text.contains("</available_skills>\n</agent_skills>"),
            "XML format should close envelope: {text}"
        );
        assert!(
            !text.contains("Path:"),
            "XML format should not contain markdown-style paths: {text}"
        );
    }

    #[test]
    fn xml_format_discovery_uses_xml_with_envelope() {
        let mut mgr = SkillManager::new();
        mgr.set_xml_format(true);
        mgr.seed(None, None, vec![], None, None, None);
        mgr.add_discovered(vec![make_skill("review", "/d/review/SKILL.md")]);
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(
            text.starts_with("<agent_skills>"),
            "XML discovery should start with envelope: {text}"
        );
        assert!(
            text.contains("<agent_skill fullPath="),
            "XML format discovery should produce <agent_skill> tags: {text}"
        );
    }

    #[test]
    fn default_format_produces_markdown() {
        let mut mgr = SkillManager::new();
        // use_xml_format defaults to false.
        mgr.seed(
            None,
            None,
            vec![make_skill("commit", "/s/commit/SKILL.md")],
            None,
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(
            text.contains("Absolute path:"),
            "default format should produce markdown: {text}"
        );
        assert!(
            !text.contains("<agent_skill"),
            "default format should not contain XML tags: {text}"
        );
    }

    // ── Session resume (restore_announced_names) tests ───────────

    #[test]
    fn restore_then_seed_produces_no_pending() {
        // Core resume test: when announced_names is restored before seed(),
        // seed() skips setting pending = BaselineChange.
        let mut mgr = SkillManager::new();
        mgr.restore_announced_names(HashSet::from(["startup".to_string()]));
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        assert!(
            mgr.take_pending_reconciliation().is_none(),
            "resume with identical skills must not produce duplicate injection"
        );
    }

    #[test]
    fn fresh_session_without_restore_produces_pending() {
        // Regression: fresh sessions (no restore) must still get their listing.
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("startup", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        let r = mgr.take_pending_reconciliation().unwrap();
        assert!(r.effects.system_reminder.is_some());
    }

    #[test]
    fn on_clear_after_restore_still_works() {
        let mut mgr = SkillManager::new();
        mgr.restore_announced_names(HashSet::from(["s".to_string()]));
        mgr.seed(
            None,
            None,
            vec![make_skill("s", "/s/SKILL.md")],
            None,
            None,
            None,
        );
        // No pending from seed (restored)
        assert!(mgr.take_pending_reconciliation().is_none());

        // /clear should still work: clears announced_names and sets pending
        mgr.on_clear();
        let r = mgr.take_pending_reconciliation().unwrap();
        assert!(r.effects.system_reminder.is_some());
    }

    #[test]
    fn discovery_after_restore_announces_new_skill() {
        let mut mgr = SkillManager::new();
        mgr.restore_announced_names(HashSet::from(["old".to_string()]));
        mgr.seed(
            None,
            None,
            vec![make_skill("old", "/s/old/SKILL.md")],
            None,
            None,
            None,
        );
        // No pending from seed
        assert!(mgr.take_pending_reconciliation().is_none());

        // Discover a new skill — should produce announcement for just the new one
        mgr.add_discovered(vec![make_skill("new", "/s/new/SKILL.md")]);
        let r = mgr.take_pending_reconciliation().unwrap();
        let text = r.effects.system_reminder.unwrap();
        assert!(text.contains("new"), "new skill should be announced");
        // "old" is already in announced_names from restore, so it should NOT appear
        assert!(
            !text.contains("- old:"),
            "old skill should not be re-announced: {text}"
        );
    }

    // ── listing_snapshot ─────────────────────────────────────────

    #[test]
    fn listing_snapshot_renders_full_set_without_mutating_announce_state() {
        let mut mgr = SkillManager::new();
        mgr.seed(
            None,
            None,
            vec![make_skill("alpha", "/s/alpha/SKILL.md")],
            None,
            None,
            None,
        );
        // Announce the baseline, then discover one more skill, so
        // `announced_names` holds both.
        let _ = mgr.take_pending_reconciliation().unwrap();
        mgr.add_discovered(vec![make_skill("beta", "/s/beta/SKILL.md")]);
        let _ = mgr.take_pending_reconciliation().unwrap();
        let announced_before = mgr.announced_names().clone();

        // The snapshot renders the whole set: announce-time dedup must not
        // apply, and the announce state must survive untouched.
        let snapshot = mgr.listing_snapshot().unwrap();
        assert_eq!(snapshot.skill_count, 2);
        assert!(snapshot.text.contains("alpha"), "{}", snapshot.text);
        assert!(snapshot.text.contains("beta"), "{}", snapshot.text);
        assert_eq!(mgr.announced_names(), &announced_before);
    }

    #[test]
    fn listing_snapshot_matches_injected_baseline_reminder() {
        // Snapshot and injection share one render path: for a fresh session
        // the snapshot text must equal the baseline announcement byte for
        // byte, in both plain and XML modes.
        for xml in [false, true] {
            let mut mgr = SkillManager::new();
            mgr.set_xml_format(xml);
            mgr.seed(
                None,
                None,
                vec![
                    make_skill("alpha", "/s/alpha/SKILL.md"),
                    make_skill("beta", "/s/beta/SKILL.md"),
                ],
                None,
                Some(128_000),
                None,
            );
            let snapshot = mgr.listing_snapshot().unwrap();
            let injected = mgr
                .take_pending_reconciliation()
                .unwrap()
                .effects
                .system_reminder
                .unwrap();
            assert_eq!(snapshot.text, injected, "xml={xml}");
        }
    }
}
