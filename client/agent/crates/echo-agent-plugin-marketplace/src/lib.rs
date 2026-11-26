//! Plugin marketplace browse and index crate.
//!
//! Provides marketplace source configuration, plugin discovery (indexed +
//! filesystem fallback), and install integration with the existing
//! `InstallRegistry` pipeline.

pub mod catalog;
pub mod config;
pub mod error;
pub mod git;
pub mod index;
pub mod install_resolve;
pub mod installer;
pub mod matcher;
pub mod scanner;
pub mod types;

pub use config::{
    env_require_sha, load_extra_sources_from_settings, load_extra_sources_from_settings_in,
    load_require_sha, load_sources,
};
pub use error::MarketplaceError;
pub use scanner::scan_marketplace;
pub use types::*;

/// Display name used for a project-owned marketplace source.
pub const OFFICIAL_SOURCE_NAME: &str = "EchoAgent Official";

/// Canonical project URL used to identify a project-owned source when configured.
///
/// The embedded runtime does not register a network marketplace automatically.
pub const OFFICIAL_SOURCE_GIT_URL: &str = "https://github.com/fuyuxiang/echo-agent-desktop.git";

/// Whether `url` is the official EchoAgent marketplace source, normalizing case, a
/// `www.` prefix, a trailing `/` or `.git`, and HTTPS/SSH forms before comparing.
pub fn is_official_source_url(url: &str) -> bool {
    canonical_github_owner_repo(url).as_deref() == Some("fuyuxiang/echo-agent-desktop")
}

/// Normalized lowercase `owner/repo` from a GitHub URL (HTTPS/http/ssh/scp,
/// `www.`, trailing `.git`/`/`), or `None` if not a GitHub URL.
pub(crate) fn canonical_github_owner_repo(url: &str) -> Option<String> {
    let s = url.trim();
    let s = s.strip_suffix('/').unwrap_or(s);
    let s = s.strip_suffix(".git").unwrap_or(s);
    let lower = s.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .or_else(|| lower.strip_prefix("ssh://"))
        .unwrap_or(&lower);
    let rest = rest.strip_prefix("git@").unwrap_or(rest);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let owner_repo = rest
        .strip_prefix("github.com/")
        .or_else(|| rest.strip_prefix("github.com:"))?;
    if owner_repo.is_empty() {
        None
    } else {
        Some(owner_repo.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_official_matches_canonical_https() {
        assert!(is_official_source_url(OFFICIAL_SOURCE_GIT_URL));
        assert!(is_official_source_url(
            "https://github.com/fuyuxiang/echo-agent-desktop"
        ));
    }

    #[test]
    fn is_official_matches_ssh_form() {
        assert!(is_official_source_url(
            "git@github.com:fuyuxiang/echo-agent-desktop.git"
        ));
        assert!(is_official_source_url(
            "git@github.com:fuyuxiang/echo-agent-desktop"
        ));
        assert!(is_official_source_url(
            "ssh://git@github.com/fuyuxiang/echo-agent-desktop.git"
        ));
        assert!(is_official_source_url(
            "ssh://git@github.com/fuyuxiang/echo-agent-desktop"
        ));
    }

    #[test]
    fn is_official_rejects_unrelated_urls() {
        assert!(!is_official_source_url(
            "https://github.com/anthropics/claude-plugins-official.git"
        ));
        assert!(!is_official_source_url(
            "https://github.com/example/other-repo.git"
        ));
        assert!(!is_official_source_url(""));
    }

    #[test]
    fn is_official_matches_noncanonical_forms() {
        assert!(is_official_source_url(
            "https://GitHub.com/Fuyuxiang/Echo-Agent-Desktop"
        ));
        assert!(is_official_source_url(
            "https://github.com/fuyuxiang/echo-agent-desktop/"
        ));
        assert!(is_official_source_url(
            "https://github.com/fuyuxiang/echo-agent-desktop.git/"
        ));
        assert!(is_official_source_url(
            "http://github.com/fuyuxiang/echo-agent-desktop"
        ));
        assert!(is_official_source_url(
            "https://www.github.com/fuyuxiang/echo-agent-desktop.git"
        ));
        assert!(is_official_source_url(
            "git@github.com:Fuyuxiang/echo-agent-desktop.git"
        ));
    }
}
