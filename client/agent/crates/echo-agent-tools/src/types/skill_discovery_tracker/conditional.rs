//! `paths:`-gated conditional skills and their per-session activation state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::implementations::skills::types::SkillInfo;

use super::canonical_path;

/// `paths:`-gated skills held out of the listing until a tool touches a matching
/// file, then activated. Session-scoped (one instance per `SkillManager`).
#[derive(Debug, Clone, Default)]
pub(super) struct ConditionalSkills {
    /// Held skills; activated ones stay here (gated by `activated`) so a reseed
    /// can re-promote them.
    held: Vec<SkillInfo>,
    /// `dedup_key`s activated this session; survives reseed.
    activated: HashSet<String>,
    /// Held skills found via dynamic discovery, so a reseed doesn't drop a hold
    /// the baseline never knew.
    dynamic_paths: HashSet<PathBuf>,
}

impl ConditionalSkills {
    pub(super) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Held skills (pending and activated), so registry-wide lookups can see
    /// skills the listing withholds.
    pub(super) fn held(&self) -> &[SkillInfo] {
        &self.held
    }

    /// A `paths:` skill that hasn't been triggered yet — withheld from the
    /// listing until a matching file is touched.
    pub(super) fn is_pending(&self, s: &SkillInfo) -> bool {
        s.paths.as_ref().is_some_and(|p| !p.is_empty()) && !self.activated.contains(&s.dedup_key())
    }

    /// Return the unconditional skills to list now, holding back the pending
    /// `paths:`-gated ones (and carrying over dynamic holds the new baseline omits).
    pub(super) fn take_unconditional(&mut self, skills: Vec<SkillInfo>) -> Vec<SkillInfo> {
        let incoming: HashSet<PathBuf> = skills.iter().map(|s| canonical_path(&s.path)).collect();
        let (mut held, unconditional): (Vec<_>, Vec<_>) =
            skills.into_iter().partition(|s| self.is_pending(s));
        for s in std::mem::take(&mut self.held) {
            let cp = canonical_path(&s.path);
            if self.dynamic_paths.contains(&cp) && !incoming.contains(&cp) {
                held.push(s);
            }
        }
        self.held = held;
        unconditional
    }

    /// Hold a dynamically discovered skill (deduped), recording its origin so a
    /// reseed keeps it.
    pub(super) fn hold_dynamic(&mut self, skill: SkillInfo) {
        let cp = canonical_path(&skill.path);
        if self.held.iter().all(|h| canonical_path(&h.path) != cp) {
            self.dynamic_paths.insert(cp);
            self.held.push(skill);
        }
    }

    /// Activate held skills matching any `touched` file. They stay held, gated
    /// by `activated`.
    pub(super) fn activate_for_paths(&mut self, touched: &[&Path], cwd: &Path) -> Vec<SkillInfo> {
        let newly: Vec<SkillInfo> = self
            .held
            .iter()
            .filter(|s| {
                !self.activated.contains(&s.dedup_key()) && skill_matches_any(s, touched, cwd)
            })
            .cloned()
            .collect();
        for s in &newly {
            self.activated.insert(s.dedup_key());
        }
        newly
    }

    /// `/clear`: drop activations and re-partition `startup` + held, so a skill a
    /// prior activation promoted into the baseline returns to held.
    pub(super) fn rehide(&mut self, mut startup: Vec<SkillInfo>) -> Vec<SkillInfo> {
        self.activated.clear();
        startup.append(&mut self.held);
        self.take_unconditional(startup)
    }
}

/// Whether any `touched` path matches the skill's `paths:` globs (gitignore,
/// relative to `cwd`). Strips the cwd prefix lexically first (works for
/// not-yet-created files); canonicalizes only on prefix mismatch (symlinked cwd).
fn skill_matches_any(skill: &SkillInfo, touched: &[&Path], cwd: &Path) -> bool {
    use ignore::gitignore::GitignoreBuilder;

    let Some(patterns) = &skill.paths else {
        return false;
    };
    let mut builder = GitignoreBuilder::new(cwd);
    for p in patterns {
        let _ = builder.add_line(None, p);
    }
    let Ok(matcher) = builder.build() else {
        return false;
    };
    // `_or_any_parents` walks up so dir patterns (e.g. `src`) match files beneath.
    touched.iter().any(|t| {
        t.strip_prefix(cwd)
            .map(Path::to_path_buf)
            .ok()
            .or_else(|| {
                dunce::canonicalize(t)
                    .ok()
                    .and_then(|c| c.strip_prefix(cwd).map(Path::to_path_buf).ok())
            })
            .is_some_and(|rel| matcher.matched_path_or_any_parents(&rel, false).is_ignore())
    })
}
