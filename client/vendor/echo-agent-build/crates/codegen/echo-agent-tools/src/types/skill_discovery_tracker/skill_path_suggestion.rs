//! Registered skill-path lookup for failed `SKILL.md` reads.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::implementations::skills::types::skill_name_from_path;

use super::{SkillManager, canonical_path};

/// A unique registered skill path matching a failed `SKILL.md` read.
#[derive(Debug, Clone)]
pub(crate) struct SkillPathSuggestion {
    /// Real path used by the filesystem backend.
    pub(crate) path: PathBuf,
    /// Model-facing path with a forked worktree prefix rewritten when needed.
    pub(crate) display_path: PathBuf,
}

impl SkillManager {
    /// Find one registered skill whose command or directory identity matches the
    /// parent directory of `requested_path`. Ambiguous matches return `None`.
    ///
    /// Candidates come from the current collections in precedence order — the
    /// listing baseline, held conditional skills, then dynamic discoveries — so
    /// a baseline reload that removes, moves, or disables a skill immediately
    /// stops suggesting it.
    pub(crate) fn suggest_skill_path(&self, requested_path: &Path) -> Option<SkillPathSuggestion> {
        let requested_name = skill_name_from_path(requested_path.to_str()?)?;
        // Fork/display state must be coherent before any path is surfaced:
        // half-seeded state could leak a real worktree path to the model.
        let display_mapping = match (&self.real_cwd_prefix, &self.display_cwd) {
            (Some(real), Some(display)) => Some((real.as_str(), display.as_str())),
            (None, None) => None,
            _ => return None,
        };

        let mut owned_paths = HashSet::new();
        let mut suggestion: Option<SkillPathSuggestion> = None;
        // Count every eligible same-name registration, including the failed
        // path itself: skipping that path without counting it would let a
        // second same-named skill look unique and get suggested.
        let mut match_count = 0usize;
        for skill in self
            .startup_skills
            .iter()
            .chain(self.conditional.held())
            .chain(&self.discovered_skills)
        {
            let skill_path = Path::new(&skill.path);
            if !skill_path.is_absolute() {
                continue;
            }
            let canonical = canonical_path(&skill.path);
            // The highest-precedence record owns its canonical path outright:
            // a shadowed record must not be suggested (nor count as ambiguity)
            // even when the owner is disabled or otherwise ineligible.
            if !owned_paths.insert(canonical.clone()) {
                continue;
            }
            if !skill.enabled || self.is_suppressed(&skill.name) {
                continue;
            }
            let Some(directory_name) = skill_name_from_path(&skill.path) else {
                continue;
            };
            if skill.name != requested_name && directory_name != requested_name {
                continue;
            }
            match_count += 1;
            if match_count > 1 {
                return None;
            }
            if canonical == requested_path {
                // Already the failed read target — not a suggestion, but it
                // still occupied the single unique-match slot above.
                continue;
            }

            let display_path = match display_mapping {
                Some((real, display)) => skill_path
                    .strip_prefix(real)
                    .map(|relative| Path::new(display).join(relative))
                    .unwrap_or_else(|_| skill_path.to_path_buf()),
                None => skill_path.to_path_buf(),
            };
            suggestion = Some(SkillPathSuggestion {
                path: skill_path.to_path_buf(),
                display_path,
            });
        }
        suggestion
    }
}

#[cfg(test)]
#[path = "skill_path_suggestion_tests.rs"]
mod tests;
