use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{self, HookSpec};
use crate::error::HookError;
use crate::event::HookEventName;
use crate::matcher::HookMatcher;

/// The loaded set of hooks, indexed by event type for fast lookup.
///
/// This is a point-in-time snapshot. Edits to hook files on disk are only
/// picked up by new sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookRegistry {
    hooks: HashMap<HookEventName, Vec<HookSpec>>,
}

impl HookRegistry {
    /// Hooks registered under the exact event key. Use
    /// [`Self::hooks_for_canonical`] for dispatch.
    pub fn hooks_for(&self, event: HookEventName) -> &[HookSpec] {
        self.hooks.get(&event).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Returns true when any enabled hook is registered for `event` or its
    /// alias spelling. Allocation-free guard for hot paths.
    pub fn has_enabled_hooks_for_canonical(&self, event: HookEventName) -> bool {
        let enabled = |specs: &[HookSpec]| {
            specs
                .iter()
                .any(|s| s.enabled && !crate::trust::is_hook_disabled(&s.name))
        };
        let canonical = event.canonical();
        enabled(self.hooks_for(canonical))
            || (canonical == HookEventName::SubagentStop
                && enabled(self.hooks_for(HookEventName::SubagentEnd)))
    }

    /// Hooks for `event` plus any registered under an alias spelling
    /// (`SubagentEnd` ≡ `SubagentStop`), so dispatch treats both identically.
    pub fn hooks_for_canonical(&self, event: HookEventName) -> Vec<&HookSpec> {
        let canonical = event.canonical();
        let mut out: Vec<&HookSpec> = self.hooks_for(canonical).iter().collect();
        if canonical == HookEventName::SubagentStop {
            out.extend(self.hooks_for(HookEventName::SubagentEnd));
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.values().all(|v| v.is_empty())
    }

    pub fn len(&self) -> usize {
        self.hooks.values().map(|v| v.len()).sum()
    }

    pub fn append_specs(&mut self, specs: Vec<HookSpec>) {
        for spec in specs {
            self.hooks.entry(spec.event).or_default().push(spec);
        }
    }

    /// Flatten the registry into a spec list in [`HookEventName::ALL`] order, so
    /// rebuilding from the result is stable regardless of `HashMap` iteration.
    pub fn into_specs(self) -> Vec<HookSpec> {
        let mut hooks = self.hooks;
        let mut out = Vec::new();
        for event in HookEventName::ALL {
            if let Some(specs) = hooks.remove(event) {
                out.extend(specs);
            }
        }
        // Defensive: `ALL` covers every variant, but keep leftovers in a stable order.
        if !hooks.is_empty() {
            let mut leftover: Vec<(HookEventName, Vec<HookSpec>)> = hooks.into_iter().collect();
            // Typed order, not `Display` (which collapses SubagentStop/SubagentEnd).
            leftover.sort_by_key(|(event, _)| *event);
            for (_, specs) in leftover {
                out.extend(specs);
            }
        }
        out
    }

    pub fn remove_by_prefix(&mut self, prefix: &str) {
        for specs in self.hooks.values_mut() {
            specs.retain(|s| !s.name.starts_with(prefix));
        }
    }

    pub fn all_hooks(&self) -> Vec<&HookSpec> {
        let mut all = Vec::new();
        for event in HookEventName::ALL {
            all.extend(self.hooks_for(*event));
        }
        all
    }

    /// Rebuild the `matcher` field (serde skips it) from `configured_matcher`
    /// after any wire restore; until then a configured pattern acts as match-all.
    /// An invalid pattern can't be rejected here (the registry is live), so it
    /// installs [`HookMatcher::never`]: fail closed rather than match all.
    pub fn recompile_matchers(&mut self) {
        for specs in self.hooks.values_mut() {
            for spec in specs.iter_mut() {
                if let Some(ref pattern) = spec.configured_matcher {
                    match HookMatcher::new(pattern) {
                        Ok(m) => spec.matcher = Some(m),
                        Err(e) => {
                            tracing::warn!(
                                hook = %spec.name,
                                pattern = %pattern,
                                error = %e,
                                "hooks: hook will match no tools until its matcher pattern is fixed"
                            );
                            // Fail closed: invalid matcher must not match-all.
                            spec.matcher = Some(HookMatcher::never());
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum HookSource<'a> {
    /// A JSON settings file; only its `hooks` key is used.
    SettingsFile(&'a Path),
    /// A directory of `*.json` hook files (e.g. `~/.echo-agent/hooks/`).
    Directory(&'a Path),
}

/// Load hooks from global and project sources.
///
/// Sources are additive; global hooks run before project. An empty registry is
/// valid.
pub fn load_hooks_from_sources(
    global_sources: &[HookSource<'_>],
    project_sources: &[HookSource<'_>],
) -> (HookRegistry, Vec<HookError>) {
    let (specs, errors) = collect_specs_from_sources(global_sources, project_sources);
    let registry = registry_from_specs_deduped(specs);
    tracing::info!(
        total_hooks = registry.len(),
        session_start = registry.hooks_for(HookEventName::SessionStart).len(),
        pre_tool = registry.hooks_for(HookEventName::PreToolUse).len(),
        post_tool = registry.hooks_for(HookEventName::PostToolUse).len(),
        session_end = registry.hooks_for(HookEventName::SessionEnd).len(),
        stop = registry.hooks_for(HookEventName::Stop).len(),
        notification = registry.hooks_for(HookEventName::Notification).len(),
        user_prompt_submit = registry.hooks_for(HookEventName::UserPromptSubmit).len(),
        subagent_start = registry.hooks_for(HookEventName::SubagentStart).len(),
        subagent_stop = registry.hooks_for(HookEventName::SubagentStop).len()
            + registry.hooks_for(HookEventName::SubagentEnd).len(),
        "hooks: discovery complete"
    );

    (registry, errors)
}

/// Load hook specs from global and project sources WITHOUT deduplicating, so a
/// caller can combine them with specs from other origins (e.g. config layers) and
/// run a single dedup pass. Global specs are prefixed `global/` and project specs
/// `project/`; global specs precede project specs so a later first-wins dedup
/// keeps the global copy of an identical duplicate.
pub fn collect_specs_from_sources(
    global_sources: &[HookSource<'_>],
    project_sources: &[HookSource<'_>],
) -> (Vec<HookSpec>, Vec<HookError>) {
    tracing::debug!(
        global_sources = global_sources.len(),
        project_sources = project_sources.len(),
        "hooks: starting discovery"
    );

    let mut all_specs = Vec::new();
    let mut all_errors = Vec::new();

    for source in global_sources {
        let (mut specs, errors) = load_from_source(source);
        for spec in &mut specs {
            spec.name = format!("{}{}", crate::config::GLOBAL_HOOK_PREFIX, spec.name);
        }
        tracing::debug!(
            source = ?source,
            count = specs.len(),
            "hooks: loaded from global source"
        );
        all_specs.extend(specs);
        all_errors.extend(errors);
    }

    for source in project_sources {
        let (mut specs, errors) = load_from_source(source);
        for spec in &mut specs {
            spec.name = format!("{}{}", crate::config::PROJECT_HOOK_PREFIX, spec.name);
        }
        tracing::debug!(
            source = ?source,
            count = specs.len(),
            "hooks: loaded from project source"
        );
        all_specs.extend(specs);
        all_errors.extend(errors);
    }

    (all_specs, all_errors)
}

/// Build a registry from specs, deduping on (canonical event, command_raw,
/// url_raw, configured_matcher) so a hook from several origins runs once; earlier
/// specs win, so callers place higher-authority first. `timeout_ms`/`extra_env`
/// are intentionally excluded from the key.
pub fn registry_from_specs_deduped(specs: Vec<HookSpec>) -> HookRegistry {
    let mut hooks: HashMap<HookEventName, Vec<HookSpec>> = HashMap::new();
    let mut seen_content: std::collections::HashSet<(HookEventName, String, String, String)> =
        std::collections::HashSet::new();
    for spec in specs {
        let key = (
            spec.event.canonical(),
            spec.command_raw.clone().unwrap_or_default(),
            spec.url_raw.clone().unwrap_or_default(),
            spec.configured_matcher.clone().unwrap_or_default(),
        );
        if seen_content.insert(key) {
            hooks.entry(spec.event).or_default().push(spec);
        } else {
            tracing::debug!(
                hook_name = %spec.name,
                event = %spec.event,
                matcher = ?spec.configured_matcher,
                "hooks: skipping duplicate hook (same content + matcher already loaded from earlier source)"
            );
        }
    }
    HookRegistry { hooks }
}

/// Convenience wrapper: load hooks from a single global directory and optional
/// project directory. Used by the existing shell integration.
pub fn load_hooks(
    global_dir: Option<&Path>,
    project_dir: Option<&Path>,
) -> (HookRegistry, Vec<HookError>) {
    let global: Vec<HookSource<'_>> = global_dir.into_iter().map(HookSource::Directory).collect();
    let project: Vec<HookSource<'_>> = project_dir.into_iter().map(HookSource::Directory).collect();
    load_hooks_from_sources(&global, &project)
}

fn load_from_source(source: &HookSource<'_>) -> (Vec<HookSpec>, Vec<HookError>) {
    match source {
        HookSource::SettingsFile(path) => load_hooks_from_settings_file(path),
        HookSource::Directory(dir) => load_hooks_from_directory(dir),
    }
}

/// Load hooks from a single JSON settings file. A missing file or absent
/// `hooks` key returns empty results, not an error.
fn load_hooks_from_settings_file(path: &Path) -> (Vec<HookSpec>, Vec<HookError>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return (Vec::new(), Vec::new());
            }
            return (
                Vec::new(),
                vec![HookError::ReadFile {
                    path: path.to_path_buf(),
                    source: e,
                }],
            );
        }
    };

    let (specs, errors) = config::parse_hook_file(&content, path);
    for err in &errors {
        tracing::warn!("hook loading from settings file: {err}");
    }
    (specs, errors)
}

fn load_hooks_from_directory(dir: &Path) -> (Vec<HookSpec>, Vec<HookError>) {
    let mut specs = Vec::new();
    let mut errors = Vec::new();

    // Best-effort listing: a bad dirent is recorded and skipped so sibling
    // hooks still load. (Sandbox fail-closed listing lives in echo_agent_config.)
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return (specs, errors);
            }
            errors.push(HookError::ReadFile {
                path: dir.to_path_buf(),
                source: e,
            });
            return (specs, errors);
        }
    };

    let mut json_files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(HookError::ReadFile {
                    path: dir.to_path_buf(),
                    source: e,
                });
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !echo_agent_config::is_direct_hook_json_name(name) || !path.is_file() {
            continue;
        }
        json_files.push(path);
    }
    json_files.sort();

    for path in json_files {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(HookError::ReadFile {
                    path: path.clone(),
                    source: e,
                });
                continue;
            }
        };

        let (file_specs, file_errors) = config::parse_hook_file(&content, &path);
        for err in &file_errors {
            tracing::warn!("hook loading: {err}");
        }
        specs.extend(file_specs);
        errors.extend(file_errors);
    }

    (specs, errors)
}

/// Check whether a path is a valid hook file (*.json, not hidden/temp).
#[cfg(test)]
fn is_valid_hook_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    echo_agent_config::is_direct_hook_json_name(name) && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_json(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn simple_hook(event: &str) -> String {
        simple_hook_with_id(event, "test")
    }

    /// A hook file whose command is keyed by `id`, so distinct ids avoid dedup.
    fn simple_hook_with_id(event: &str, id: &str) -> String {
        serde_json::json!({
            "hooks": {
                event: [{"hooks": [{"type": "command", "command": format!("{}.sh", id)}]}]
            }
        })
        .to_string()
    }

    /// Drift guard: gate events must match the `blockingEvents` the agent
    /// advertises (extensions/hooks.rs). A new gate event fails here.
    #[test]
    fn gate_events_are_the_known_set() {
        use crate::event::GateKind;
        // Canonicalize first to dedup alias spellings into one set entry
        // (`traits()` itself already canonicalizes, so it's safe on aliases).
        let gates: std::collections::HashSet<_> = HookEventName::ALL
            .iter()
            .map(|e| e.canonical())
            .filter(|e| e.traits().gate != GateKind::Observe)
            .collect();
        let expected: std::collections::HashSet<_> = [
            HookEventName::PreToolUse,
            HookEventName::Stop,
            HookEventName::SubagentStop,
        ]
        .into_iter()
        .collect();
        assert_eq!(gates, expected, "gate events changed");
    }

    #[test]
    fn load_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty());
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn load_nonexistent_dir() {
        let (registry, errors) = load_hooks(Some(Path::new("/nonexistent/path/hooks")), None);
        assert!(errors.is_empty()); // NotFound is silent
        assert!(registry.is_empty());
    }

    #[test]
    fn load_single_hook() {
        let dir = tempfile::tempdir().unwrap();
        write_json(dir.path(), "safety.json", &simple_hook("PreToolUse"));

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(registry.len(), 1);
        let hooks = registry.hooks_for(HookEventName::PreToolUse);
        assert_eq!(hooks.len(), 1);
    }

    #[test]
    fn lexicographic_ordering_across_files() {
        let dir = tempfile::tempdir().unwrap();
        write_json(
            dir.path(),
            "02-second.json",
            &simple_hook_with_id("PreToolUse", "second"),
        );
        write_json(
            dir.path(),
            "01-first.json",
            &simple_hook_with_id("PreToolUse", "first"),
        );
        write_json(
            dir.path(),
            "03-third.json",
            &simple_hook_with_id("PreToolUse", "third"),
        );

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::PreToolUse);
        let commands: Vec<_> = hooks.iter().map(|h| h.command_raw.as_deref()).collect();
        assert_eq!(
            commands,
            [Some("first.sh"), Some("second.sh"), Some("third.sh")],
            "hooks must load in lexicographic file order (01-, 02-, 03-)"
        );
    }

    #[test]
    fn global_before_project() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_json(
            global.path(),
            "global.json",
            &simple_hook_with_id("PreToolUse", "global"),
        );
        write_json(
            project.path(),
            "project.json",
            &simple_hook_with_id("PreToolUse", "project"),
        );

        let (registry, errors) = load_hooks(Some(global.path()), Some(project.path()));
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::PreToolUse);
        assert_eq!(hooks.len(), 2);
    }

    #[test]
    fn skip_hidden_and_non_json_files() {
        let dir = tempfile::tempdir().unwrap();
        write_json(dir.path(), "valid.json", &simple_hook("SessionStart"));
        write_json(dir.path(), ".hidden.json", &simple_hook("SessionStart"));
        write_json(dir.path(), "backup.json~", "{}");
        write_json(dir.path(), "not-json.txt", "{}");
        write_json(dir.path(), "not-json.toml", "version = 1");

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn multiple_handlers_in_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"{
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "a.sh" },
                            { "type": "command", "command": "b.sh" }
                        ]
                    }
                ]
            }
        }"#;
        write_json(dir.path(), "multi.json", content);

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::PreToolUse);
        assert_eq!(hooks.len(), 2);
    }

    #[test]
    fn invalid_file_skipped_others_loaded() {
        let dir = tempfile::tempdir().unwrap();
        write_json(dir.path(), "01-good.json", &simple_hook("SessionStart"));
        write_json(dir.path(), "02-bad.json", "not valid json {{{");
        write_json(dir.path(), "03-also-good.json", &simple_hook("SessionEnd"));

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], HookError::ParseFile { .. }));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn all_hooks_covers_every_event_type() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"{
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": "a.sh"}]}],
                "PreToolUse": [{"hooks": [{"type": "command", "command": "b.sh"}]}],
                "PostToolUse": [{"hooks": [{"type": "command", "command": "c.sh"}]}],
                "SessionEnd": [{"hooks": [{"type": "command", "command": "d.sh"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "e.sh"}]}],
                "Notification": [{"hooks": [{"type": "command", "command": "f.sh"}]}],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "g.sh"}]}],
                "SubagentStart": [{"hooks": [{"type": "command", "command": "h.sh"}]}],
                "SubagentStop": [{"hooks": [{"type": "command", "command": "i.sh"}]}],
                "SubagentEnd": [{"hooks": [{"type": "command", "command": "j.sh"}]}]
            }
        }"#;
        write_json(dir.path(), "all-events.json", content);

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(registry.len(), 10);

        let all = registry.all_hooks();
        let events: std::collections::HashSet<_> = all.iter().map(|h| h.event).collect();
        assert_eq!(
            events.len(),
            10,
            "all_hooks() must cover 10 distinct event types"
        );
    }

    #[test]
    fn is_valid_hook_file_cases() {
        let dir = tempfile::tempdir().unwrap();

        let valid = dir.path().join("hooks.json");
        std::fs::write(&valid, "").unwrap();
        assert!(is_valid_hook_file(&valid));

        let hidden = dir.path().join(".hidden.json");
        std::fs::write(&hidden, "").unwrap();
        assert!(!is_valid_hook_file(&hidden));

        let backup = dir.path().join("backup.json~");
        std::fs::write(&backup, "").unwrap();
        assert!(!is_valid_hook_file(&backup));

        let txt = dir.path().join("readme.txt");
        std::fs::write(&txt, "").unwrap();
        assert!(!is_valid_hook_file(&txt));

        let toml = dir.path().join("hooks.toml");
        std::fs::write(&toml, "").unwrap();
        assert!(!is_valid_hook_file(&toml)); // TOML no longer accepted
    }

    #[test]
    fn load_from_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"check.sh"}]}]}}"#,
        )
        .unwrap();

        let (registry, errors) =
            load_hooks_from_sources(&[HookSource::SettingsFile(&settings)], &[]);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn load_from_missing_settings_file() {
        let (registry, errors) = load_hooks_from_sources(
            &[HookSource::SettingsFile(Path::new(
                "/nonexistent/settings.json",
            ))],
            &[],
        );
        assert!(errors.is_empty()); // Missing file is fine, not an error.
        assert!(registry.is_empty());
    }

    #[test]
    fn load_from_settings_file_no_hooks_key() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, r#"{"theme": "dark", "model": "echo-agent-3"}"#).unwrap();

        let (registry, errors) =
            load_hooks_from_sources(&[HookSource::SettingsFile(&settings)], &[]);
        assert!(errors.is_empty());
        assert!(registry.is_empty());
    }

    #[test]
    fn mixed_sources_settings_and_directory() {
        let dir = tempfile::tempdir().unwrap();

        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"from-settings.sh"}]}]}}"#,
        )
        .unwrap();

        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        write_json(&hooks_dir, "extra.json", &simple_hook("SessionStart"));

        let (registry, errors) = load_hooks_from_sources(
            &[
                HookSource::SettingsFile(&settings),
                HookSource::Directory(&hooks_dir),
            ],
            &[],
        );
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.hooks_for(HookEventName::PreToolUse).len(), 1);
        assert_eq!(registry.hooks_for(HookEventName::SessionStart).len(), 1);
    }

    #[test]
    fn global_and_project_settings_merged() {
        let dir = tempfile::tempdir().unwrap();

        let global_settings = dir.path().join("global.json");
        std::fs::write(
            &global_settings,
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"global.sh"}]}]}}"#,
        )
        .unwrap();

        let project_settings = dir.path().join("project.json");
        std::fs::write(
            &project_settings,
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"project.sh"}]}]}}"#,
        )
        .unwrap();

        let (registry, errors) = load_hooks_from_sources(
            &[HookSource::SettingsFile(&global_settings)],
            &[HookSource::SettingsFile(&project_settings)],
        );
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::PreToolUse);
        assert_eq!(hooks.len(), 2);
        assert!(hooks[0].name.starts_with("global/"));
        assert!(hooks[1].name.starts_with("project/"));
    }

    #[test]
    fn deduplicates_hooks_with_same_content_across_sources() {
        let dir = tempfile::tempdir().unwrap();

        let global_settings = dir.path().join("global.json");
        std::fs::write(
            &global_settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"safety.sh"}]}]}}"#,
        )
        .unwrap();

        let claude_settings = dir.path().join("claude.json");
        std::fs::write(
            &claude_settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"safety.sh"}]}]}}"#,
        )
        .unwrap();

        let cursor_settings = dir.path().join("cursor.json");
        std::fs::write(
            &cursor_settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"safety.sh"}]}]}}"#,
        )
        .unwrap();

        let (registry, errors) = load_hooks_from_sources(
            &[
                HookSource::SettingsFile(&global_settings),
                HookSource::SettingsFile(&claude_settings),
                HookSource::SettingsFile(&cursor_settings),
            ],
            &[],
        );
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::SessionStart);
        assert_eq!(
            hooks.len(),
            1,
            "expected exactly 1 SessionStart hook after dedup, got {}",
            hooks.len()
        );
        assert!(
            hooks[0].name.starts_with("global/"),
            "first source (global) should win, got: {}",
            hooks[0].name
        );
    }

    /// A hook registered under both `SubagentStop` and `SubagentEnd` dedups on
    /// the canonical event, so it runs once.
    #[test]
    fn deduplicates_hooks_across_alias_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"hooks":{
                "SubagentStop":[{"hooks":[{"type":"command","command":"notify.sh"}]}],
                "SubagentEnd":[{"hooks":[{"type":"command","command":"notify.sh"}]}]
            }}"#,
        )
        .unwrap();

        let (registry, errors) =
            load_hooks_from_sources(&[HookSource::SettingsFile(&settings)], &[]);
        assert!(errors.is_empty());
        assert_eq!(
            registry
                .hooks_for_canonical(HookEventName::SubagentStop)
                .len(),
            1,
            "alias spelling must not double-register the same hook"
        );
    }

    #[test]
    fn different_commands_not_deduplicated() {
        let dir = tempfile::tempdir().unwrap();

        let global_settings = dir.path().join("global.json");
        std::fs::write(
            &global_settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"first.sh"}]}]}}"#,
        )
        .unwrap();

        let claude_settings = dir.path().join("claude.json");
        std::fs::write(
            &claude_settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"second.sh"}]}]}}"#,
        )
        .unwrap();

        let (registry, errors) = load_hooks_from_sources(
            &[
                HookSource::SettingsFile(&global_settings),
                HookSource::SettingsFile(&claude_settings),
            ],
            &[],
        );
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::SessionStart);
        assert_eq!(
            hooks.len(),
            2,
            "expected 2 SessionStart hooks with different commands, got {}",
            hooks.len()
        );
    }

    #[test]
    fn different_event_types_not_deduplicated() {
        let dir = tempfile::tempdir().unwrap();

        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{
                "hooks": {
                    "SessionStart": [{"hooks": [{"type": "command", "command": "hook.sh"}]}],
                    "SessionEnd": [{"hooks": [{"type": "command", "command": "hook.sh"}]}]
                }
            }"#,
        )
        .unwrap();

        let (registry, errors) =
            load_hooks_from_sources(&[HookSource::SettingsFile(&settings)], &[]);
        assert!(errors.is_empty());
        assert_eq!(registry.hooks_for(HookEventName::SessionStart).len(), 1);
        assert_eq!(registry.hooks_for(HookEventName::SessionEnd).len(), 1);
    }

    /// The same command in multiple files within one directory dedups to a
    /// single run, preventing accidental duplicate execution.
    #[test]
    fn same_command_in_same_directory_deduplicated() {
        let dir = tempfile::tempdir().unwrap();

        write_json(
            dir.path(),
            "01-first.json",
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"same.sh"}]}]}}"#,
        );
        write_json(
            dir.path(),
            "02-second.json",
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"same.sh"}]}]}}"#,
        );

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(errors.is_empty());
        let hooks = registry.hooks_for(HookEventName::SessionStart);
        assert_eq!(
            hooks.len(),
            1,
            "expected exactly 1 SessionStart hook after dedup, got {}",
            hooks.len()
        );
    }

    #[test]
    fn realistic_claude_settings_discovery() {
        let dir = tempfile::tempdir().unwrap();

        let claude_settings = dir.path().join("settings.json");
        std::fs::write(
            &claude_settings,
            r#"{
                "model": "claude-sonnet-4-20250514",
                "permissions": {"allow": ["Bash(npm test)"]},
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Bash", "hooks": [{"type": "command", "command": "check.sh"}]}
                    ]
                },
                "mcpServers": {"memory": {"command": "npx"}}
            }"#,
        )
        .unwrap();

        let (registry, errors) =
            load_hooks_from_sources(&[HookSource::SettingsFile(&claude_settings)], &[]);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(registry.len(), 1);
    }

    /// Wire/serde-shaped spec: compiled matcher cleared, pattern still set.
    fn recompile_test_spec(
        name: &str,
        configured_matcher: Option<&str>,
    ) -> crate::config::HookSpec {
        use std::path::PathBuf;
        crate::config::HookSpec {
            name: name.into(),
            event: HookEventName::PreToolUse,
            handler_type: crate::config::HandlerType::Command,
            configured_matcher: configured_matcher.map(str::to_owned),
            matcher: None,
            enabled: true,
            command: Some(PathBuf::from("hook.sh")),
            command_raw: Some("hook.sh".into()),
            url: None,
            url_raw: None,
            timeout_ms: 5_000,
            source_dir: PathBuf::from("/tmp"),
            extra_env: Default::default(),
            layer: crate::config::HookProvenance::File,
        }
    }

    #[test]
    fn recompile_matchers_leaves_intentional_match_all() {
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![recompile_test_spec("all", None)]);
        registry.recompile_matchers();

        assert!(
            registry.hooks_for(HookEventName::PreToolUse)[0]
                .matcher
                .is_none(),
            "no configured pattern must stay match-all (matcher None)"
        );
    }

    #[test]
    fn recompile_matchers_isolates_invalid_sibling() {
        let mut registry = HookRegistry::default();
        registry.append_specs(vec![
            recompile_test_spec("ok", Some("Bash")),
            recompile_test_spec("broken", Some("[invalid")),
        ]);
        registry.recompile_matchers();

        let hooks = registry.hooks_for(HookEventName::PreToolUse);
        assert_eq!(hooks.len(), 2);
        let by_name: std::collections::HashMap<_, _> =
            hooks.iter().map(|h| (h.name.as_str(), h)).collect();

        let ok = by_name["ok"]
            .matcher
            .as_ref()
            .expect("valid sibling must recompile");
        assert!(ok.is_match("run_terminal_command"));
        assert!(!ok.is_match("read_file"));

        let broken = by_name["broken"]
            .matcher
            .as_ref()
            .expect("invalid sibling must become never-match");
        assert!(!broken.is_match("run_terminal_command"));
        assert!(!broken.is_match("Bash"));
        assert!(!broken.is_match("read_file"));
    }
}
