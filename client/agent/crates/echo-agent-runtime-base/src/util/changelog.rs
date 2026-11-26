//! Changelog fetching from CDN with local disk cache.
//!
//! Both markdown (`*.external.md`) and JSON (`*.external.json`) changelogs
//! are published per-version to the CDN at `echo.agent/cli/changelogs/`.
//!
//! `ChangelogManager::fetch()` retrieves both formats in parallel and
//! returns a `Changelog` with optional markdown + structured entries.
//! Consumers pick the format they need:
//! - `/release-notes` uses `changelog.markdown` for rich scrollback display
//! - Welcome screen uses `changelog.entries` for bullet rendering

use std::path::PathBuf;

/// CDN base for all changelogs (proxies to GCS, cache-friendly).
const CHANGELOG_BASE: &str = "https://echo-agent.invalid/cli/changelogs";
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A single structured changelog entry from the published JSON changelog.
///
/// Shape must match the output of `render_external_json` in `changelog.sh`:
///   `{category, description, breaking_change}`
/// If you change fields here, update `changelog.sh:render_external_json` too.
///
/// All fields use `#[serde(default)]` so a single malformed entry doesn't
/// kill the entire array parse. Entries with an empty description are
/// filtered out by `bullets_from_entries`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChangelogEntry {
    /// Category label (e.g. "features", "fixes", "breaking", "performance").
    #[serde(default)]
    pub category: String,
    /// Human-readable description (may contain `**bold**` or backticks).
    #[serde(default)]
    pub description: String,
    /// Whether this entry represents a breaking change.
    #[serde(default)]
    pub breaking_change: bool,
}

/// Both formats of a version's changelog, fetched together.
pub struct Changelog {
    /// Rendered markdown (for `/release-notes` display).
    pub markdown: Option<String>,
    /// Structured entries (for welcome screen bullets).
    pub entries: Option<Vec<ChangelogEntry>>,
}

/// Manages changelog retrieval from CDN with local disk caching.
///
/// Single entry point: `fetch()` returns both markdown and JSON in one
/// `Changelog` struct. Each format is fetched independently with its own
/// cache file, so a failure in one doesn't block the other.
pub struct ChangelogManager {
    md_cache: PathBuf,
    json_cache: PathBuf,
}

impl Default for ChangelogManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ChangelogManager {
    pub fn new() -> Self {
        // Prefer live `$ECHO_AGENT_HOME` so harness-injected homes (PTY e2e) always
        // win over a OnceLock that may have been initialised earlier with a
        // different path in the same process graph.
        Self::from_env_home()
    }

    /// Resolve cache paths from the live process environment (not the
    /// `echo_agent_home()` OnceLock). A seeded `$ECHO_AGENT_HOME` set on the pager
    /// process is always honoured even if some earlier init path cached a
    /// different home.
    fn from_env_home() -> Self {
        let home = std::env::var_os("ECHO_AGENT_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(crate::util::echo_agent_home::echo_agent_home);
        Self {
            md_cache: home.join("CHANGELOG.md"),
            json_cache: home.join("CHANGELOG.json"),
        }
    }

    /// Fetch both markdown and JSON changelogs for the current version.
    ///
    /// Each format is fetched independently (CDN, 3 s timeout) and cached
    /// to disk. On failure, falls back to the cached copy. Either field
    /// may be `None` if offline with no cache.
    ///
    /// When `ECHO_AGENT_CHANGELOG_OFFLINE` is set (PTY / integration tests), skip
    /// the CDN entirely and read only the disk cache so seeded fixtures win
    /// deterministically without network races. Paths are re-resolved from
    /// `$ECHO_AGENT_HOME` so harness-injected env always applies.
    ///
    /// JSON is only cached after a successful parse to avoid poisoning the
    /// disk cache with malformed content (the markdown cache is write-through
    /// since it's consumed as raw text).
    pub fn fetch(&self) -> Changelog {
        // Always re-resolve from env so a caller holding an older manager
        // (or OnceLock lag) still reads the live harness home.
        Self::from_env_home().fetch_with(changelog_offline(), CHANGELOG_BASE)
    }

    /// Fetch using this manager's already-resolved cache paths, an explicit
    /// offline flag, and an explicit CDN base.
    ///
    /// Split out of [`fetch`] so unit tests can drive it against a temp home
    /// without mutating process-global env (`ECHO_AGENT_HOME` /
    /// `ECHO_AGENT_CHANGELOG_OFFLINE`), which races across the parallel test
    /// harness. Passing an unreachable `base` lets a test force a
    /// deterministic CDN miss instead of depending on whether the sandbox
    /// happens to block network. Production callers always go through
    /// [`fetch`], so behaviour is unchanged.
    fn fetch_with(&self, offline: bool, base: &str) -> Changelog {
        if offline {
            return Changelog {
                markdown: read_cache(&self.md_cache),
                entries: self.read_json_cache(),
            };
        }

        let version = echo_agent_version::VERSION;
        let md_url = format!("{}/{}.external.md", base, version);

        // Fetch both formats in parallel (3s timeout each → 3s total, not 6s).
        let mut markdown = None;
        let mut entries = None;
        std::thread::scope(|s| {
            let md_handle = s.spawn(|| self.fetch_and_cache(&md_url, &self.md_cache));
            let json_handle = s.spawn(|| self.fetch_json(base, version));
            markdown = md_handle.join().ok().flatten();
            entries = json_handle.join().ok().flatten();
        });

        // If CDN is unreachable (CI sandboxes, airplane mode), fall back to
        // any on-disk seed under `$ECHO_AGENT_HOME` even when offline mode was not
        // explicitly requested — keeps PTY/integration tests deterministic.
        if markdown.is_none() {
            markdown = read_cache(&self.md_cache);
        }
        if entries.is_none() {
            entries = self.read_json_cache();
        }

        Changelog { markdown, entries }
    }

    /// Fetch and parse JSON changelog, caching only after successful parse.
    fn fetch_json(&self, base: &str, version: &str) -> Option<Vec<ChangelogEntry>> {
        let url = format!("{}/{}.external.json", base, version);

        // Try remote first — only cache after successful parse.
        if let Ok(raw) = fetch_blocking(&url)
            && !raw.trim().is_empty()
        {
            match serde_json::from_str::<Vec<ChangelogEntry>>(&raw) {
                Ok(entries) => {
                    if let Err(e) = std::fs::write(&self.json_cache, &raw) {
                        tracing::debug!(error = %e, "JSON changelog cache write failed");
                    }
                    return Some(entries);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "failed to parse JSON changelog from CDN");
                }
            }
        }

        self.read_json_cache()
    }

    fn read_json_cache(&self) -> Option<Vec<ChangelogEntry>> {
        let cached = read_cache(&self.json_cache)?;
        match serde_json::from_str(&cached) {
            Ok(entries) => Some(entries),
            Err(e) => {
                tracing::debug!(error = %e, "failed to parse cached JSON changelog");
                None
            }
        }
    }

    /// Shared fetch-and-cache: try remote (3 s timeout), cache on success,
    /// fall back to disk cache on failure.
    fn fetch_and_cache(&self, url: &str, cache_path: &std::path::Path) -> Option<String> {
        if let Ok(content) = fetch_blocking(url)
            && !content.trim().is_empty()
        {
            if let Err(e) = std::fs::write(cache_path, &content) {
                tracing::debug!(error = %e, path = %cache_path.display(), "cache write failed");
            }
            return Some(content);
        }
        read_cache(cache_path)
    }
}

/// When set, `ChangelogManager::fetch` skips the CDN and only reads disk cache.
/// Used by PTY harness tests that seed `CHANGELOG.{md,json}` under a temp home.
fn changelog_offline() -> bool {
    std::env::var_os("ECHO_AGENT_CHANGELOG_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0")
}

fn read_cache(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|c| !c.trim().is_empty())
}

/// Strip `**bold**` markers and backticks from a description string.
fn strip_markdown_inline(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

/// Convert changelog entries to plain-text bullet strings.
///
/// Strips `**bold**` and backtick formatting from each description,
/// skips entries with empty descriptions (from tolerant deserialization),
/// and returns at most `max` entries.
pub fn bullets_from_entries(entries: &[ChangelogEntry], max: usize) -> Vec<String> {
    entries
        .iter()
        .filter(|e| !e.description.is_empty())
        .take(max)
        .map(|e| strip_markdown_inline(&e.description))
        .collect()
}

/// Blocking HTTP fetch. Callers (`std::thread::scope` threads) are already
/// off the tokio runtime, so no extra thread spawn is needed.
fn fetch_blocking(url: &str) -> anyhow::Result<String> {
    let client = echo_agent_extra_ca::build_blocking_reqwest_client(|builder| {
        builder.timeout(FETCH_TIMEOUT)
    })?;
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    Ok(resp.text()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a manager pointing at `home` directly, bypassing the global
    /// `$ECHO_AGENT_HOME` env so tests never race the parallel harness.
    fn manager_for(home: &std::path::Path) -> ChangelogManager {
        ChangelogManager {
            md_cache: home.join("CHANGELOG.md"),
            json_cache: home.join("CHANGELOG.json"),
        }
    }

    #[test]
    fn offline_mode_reads_seeded_disk_cache_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("echo-agent-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("CHANGELOG.md"), "# seeded offline md\n").unwrap();
        std::fs::write(
            home.join("CHANGELOG.json"),
            r#"[{"category":"features","description":"seeded entry","breaking_change":false}]"#,
        )
        .unwrap();

        // Offline path: read only the seeded disk cache, no network.
        let changelog = manager_for(&home).fetch_with(true, CHANGELOG_BASE);
        assert_eq!(
            changelog.markdown.as_deref(),
            Some("# seeded offline md\n"),
            "offline mode must return seeded markdown"
        );
        let entries = changelog.entries.expect("seeded json entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].description, "seeded entry");
    }

    #[test]
    fn cdn_miss_falls_back_to_env_home_disk_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("echo-agent-home-fallback");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("CHANGELOG.md"), "# fallback md\n").unwrap();

        // Non-offline path with an unreachable CDN base: the remote fetch
        // fails deterministically (no dependency on the sandbox blocking
        // network), so the on-disk cache must win.
        let changelog = manager_for(&home).fetch_with(false, "http://127.0.0.1:1");
        assert_eq!(
            changelog.markdown.as_deref(),
            Some("# fallback md\n"),
            "CDN miss must fall back to the seeded CHANGELOG.md"
        );
    }

    #[test]
    fn bullets_strips_markdown_and_respects_max() {
        let entries = vec![
            ChangelogEntry {
                category: "features".into(),
                description: "Added **dark mode** support".into(),
                breaking_change: false,
            },
            ChangelogEntry {
                category: "fixes".into(),
                description: "Fixed `crash` on startup".into(),
                breaking_change: false,
            },
            ChangelogEntry {
                category: "performance".into(),
                description: "Faster **rendering** of `code` blocks".into(),
                breaking_change: false,
            },
        ];

        let bullets = bullets_from_entries(&entries, 2);
        assert_eq!(bullets.len(), 2);
        assert_eq!(bullets[0], "Added dark mode support");
        assert_eq!(bullets[1], "Fixed crash on startup");
    }

    #[test]
    fn bullets_skips_empty_descriptions() {
        let entries = vec![
            ChangelogEntry {
                category: "features".into(),
                description: "Good entry".into(),
                breaking_change: false,
            },
            ChangelogEntry {
                category: String::new(),
                description: String::new(), // bad entry from tolerant deser
                breaking_change: false,
            },
            ChangelogEntry {
                category: "fixes".into(),
                description: "Another good one".into(),
                breaking_change: false,
            },
        ];
        let bullets = bullets_from_entries(&entries, 10);
        assert_eq!(bullets, vec!["Good entry", "Another good one"]);
    }

    #[test]
    fn tolerant_deserialization_partial_entry() {
        // Missing description field → defaults to empty string, not a parse error
        let json = r#"[{"category":"features"},{"description":"ok"}]"#;
        let entries: Vec<ChangelogEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].description, "");
        assert_eq!(entries[1].category, "");
        assert_eq!(entries[1].description, "ok");
    }
}
