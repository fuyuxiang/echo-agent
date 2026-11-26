//! Advisory image capability tokens, declared by the sandbox image as marker
//! files under `/usr/share/echoagent/capabilities.d/<token>`.
//!
//! ADVISORY ONLY: never an authorization input. Guest processes run as root
//! and can create files here at will. Deliberately NOT under `/etc/echoagent`,
//! which is the root-owned permission-policy tier.

use std::sync::OnceLock;

use tracing::{info, warn};

/// Default in-image declaration directory (image content tier, matching
/// `/usr/share/echoagent/bundled-skills`).
pub const DEFAULT_CAPABILITIES_DIR: &str = "/usr/share/echoagent/capabilities.d";
/// Override for tests / local dev / non-Linux hosts.
pub const CAPABILITIES_DIR_ENV: &str = "ECHO_AGENT_IMAGE_CAPABILITIES_DIR";
/// Re-exported so the reader and the bind reply that carries its output
/// cannot drift on the spelling.
pub use echo_agent_tool_protocol::IMAGE_CAPABILITIES_V1;
// The gate and the caps live beside the wire field they bound: every hop that
// validates the set must apply identical rules.
use echo_agent_tool_protocol::{MAX_IMAGE_CAPABILITIES, is_image_capability_token};
/// Guest-writable directory, so bound the scan itself and not just its result.
const MAX_SCANNED_ENTRIES: usize = 1024;
/// Cap on names listed in the drop-`warn!`; the count is always exact.
const MAX_LOGGED_REJECTIONS: usize = 8;

/// Read outcome, distinguishing "unknown" from "declares nothing".
#[derive(Debug, Clone, Default)]
pub struct ImageCapabilities {
    /// Sorted, deduped, validated tokens. Independent of `declared`: a child
    /// image built on a pre-token parent declares its own tokens but no
    /// `capabilities.v1`.
    tokens: Vec<String>,
    /// `true` iff the directory was read in full and declared
    /// `capabilities.v1`. Gates [`ImageCapabilities::state`] only, not
    /// [`ImageCapabilities::wire`].
    declared: bool,
}

impl ImageCapabilities {
    /// `None` = unknown (no declaration); `Some(false)` = declared-and-absent.
    pub fn state(&self, token: &str) -> Option<bool> {
        self.declared.then(|| {
            self.tokens
                .binary_search_by(|candidate| candidate.as_str().cmp(token))
                .is_ok()
        })
    }

    /// Fail-closed convenience: unknown ⇒ false.
    pub fn has(&self, token: &str) -> bool {
        self.state(token).unwrap_or(false)
    }

    pub fn is_declared(&self) -> bool {
        self.declared
    }

    /// Sorted validated tokens, returned even when `!declared`, so the tokens
    /// still travel and the caller re-derives UNKNOWN from the missing
    /// self-token instead of losing the diagnostic content.
    ///
    /// Values are guest-forgeable and unbounded in cardinality across sessions:
    /// safe as a log field, a Mongo array element or rendered text; never as a
    /// metric label value or a map key.
    pub fn wire(&self) -> &[String] {
        &self.tokens
    }
}

/// Process-wide cache, primed at workspace-server startup; the directory is
/// read exactly once. A later read would let a guest flip a gate mid-session.
/// Re-exec and `Restore` launches read after the container has run guest code.
pub fn image_capabilities() -> &'static ImageCapabilities {
    static CACHE: OnceLock<ImageCapabilities> = OnceLock::new();
    CACHE.get_or_init(|| {
        let dir = capabilities_dir_from_raw(std::env::var(CAPABILITIES_DIR_ENV).ok());
        let caps = load_from_dir(&dir);
        if caps.declared {
            info!(dir = ?dir, tokens = caps.tokens.len(),
                capabilities = ?caps.tokens, "image capability declaration read");
        } else if caps.tokens.is_empty() {
            // NOT "no capabilities": the image predates the scheme, the dir is
            // unreadable, or this is a non-Linux host. Gated features fail closed.
            info!(dir = ?dir,
                "no image capability declaration found; capabilities UNKNOWN, gated features off");
        } else {
            // Tokens were read but cannot answer "token X is absent": either
            // the set went over the cap or `capabilities.v1` is missing. The
            // preceding `warn!` says which.
            info!(dir = ?dir, tokens = caps.tokens.len(), capabilities = ?caps.tokens,
                "image capability tokens read but not authoritative; capabilities UNKNOWN, \
                 gated features off");
        }
        caps
    })
}

/// Pure resolution of the directory override: a non-blank value wins, anything
/// else falls back to [`DEFAULT_CAPABILITIES_DIR`].
fn capabilities_dir_from_raw(raw: Option<String>) -> String {
    raw.filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CAPABILITIES_DIR.to_owned())
}

/// `read_dir` → filter `is_file()` → validate → sort/dedup → cap at
/// [`MAX_IMAGE_CAPABILITIES`]. A listing that cannot be read in full (an
/// iterator error, an entry that cannot be classified, or over
/// `MAX_SCANNED_ENTRIES`) yields the default: no tokens, `declared = false`.
/// Going over [`MAX_IMAGE_CAPABILITIES`], or a non-empty set without
/// `capabilities.v1`, keeps the tokens and still sets `declared = false`.
fn load_from_dir(dir: &str) -> ImageCapabilities {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return ImageCapabilities::default(),
    };

    let mut tokens: Vec<String> = Vec::new();
    let mut rejected = Vec::new();
    let mut rejected_count = 0usize;
    let mut scanned = 0usize;
    // What a partial scan saw is a filesystem-order prefix, not a property of
    // the image, so every incomplete-read exit below discards it: such a read
    // can neither answer "token X is absent" nor publish a readable token
    // list. The cap below keeps its tokens: those are a deterministic subset.
    for entry in entries {
        let Ok(entry) = entry else {
            warn!(dir = ?dir,
                "image capability directory listing failed mid-scan; tokens discarded, \
                 capabilities UNKNOWN");
            return ImageCapabilities::default();
        };
        scanned += 1;
        if scanned > MAX_SCANNED_ENTRIES {
            warn!(dir = ?dir, cap = MAX_SCANNED_ENTRIES,
                "image capability directory too large; scan truncated, tokens discarded, \
                 capabilities UNKNOWN");
            return ImageCapabilities::default();
        }
        // `file_type()` deliberately does not follow symlinks: markers are
        // regular files created by `:` redirection in the image build. An
        // error here means the entry could not be classified, which is an
        // incomplete read rather than a non-marker, so it discards the scan.
        let Ok(file_type) = entry.file_type() else {
            warn!(dir = ?dir,
                "image capability marker could not be classified; tokens discarded, \
                 capabilities UNKNOWN");
            return ImageCapabilities::default();
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_image_capability_token(&name) {
            tokens.push(name);
        } else if is_rejection_noteworthy(&name) {
            rejected_count += 1;
            if rejected.len() < MAX_LOGGED_REJECTIONS {
                rejected.push(name);
            }
        }
    }

    if rejected_count > 0 {
        warn!(dir = ?dir, count = rejected_count, names = ?rejected,
            "dropped suspicious image capability marker names (stray dotfiles and dot-less files \
             are dropped silently and not counted)");
    }

    tokens.sort_unstable();
    tokens.dedup();

    let mut truncated = false;
    if tokens.len() > MAX_IMAGE_CAPABILITIES {
        truncated = true;
        // Trim the lexicographic tail so the retained set is deterministic.
        let dropped = tokens.len() - MAX_IMAGE_CAPABILITIES;
        tokens.truncate(MAX_IMAGE_CAPABILITIES);
        warn!(dir = ?dir, count = dropped, cap = MAX_IMAGE_CAPABILITIES,
            "image capability marker count over cap; extra tokens dropped, capabilities UNKNOWN");
    }

    let self_declared = tokens.iter().any(|t| t == IMAGE_CAPABILITIES_V1);
    if !self_declared && !tokens.is_empty() {
        warn!(dir = ?dir, tokens = tokens.len(),
            "image declares capability tokens but not `capabilities.v1`; treating as UNKNOWN. \
             Rebuild the image on a token-aware base");
    }

    // A truncated read cannot answer "token X is absent" authoritatively.
    let declared = self_declared && !truncated;
    ImageCapabilities { tokens, declared }
}

/// Whether a rejected entry deserves the drop-`warn!`. Dotfiles and dot-less
/// names are stray editor/OS junk, never botched tokens. Warning on them would
/// fire every session and stop meaning anything.
fn is_rejection_noteworthy(name: &str) -> bool {
    !name.starts_with('.') && name.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tool_protocol::MAX_IMAGE_CAPABILITY_LEN;
    use std::fs;
    use std::path::Path;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"").unwrap();
    }

    fn load(dir: &Path) -> ImageCapabilities {
        load_from_dir(dir.to_str().unwrap())
    }

    #[test]
    fn missing_dir_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let caps = load(&tmp.path().join("absent"));
        assert!(!caps.is_declared());
        assert!(caps.wire().is_empty());
        assert_eq!(caps.state("echo-agent-files.occ"), None);
        assert!(!caps.has("echo-agent-files.occ"));
    }

    #[test]
    fn empty_declared_dir_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let caps = load(tmp.path());
        assert!(!caps.is_declared());
        assert!(caps.wire().is_empty());
    }

    #[test]
    fn self_token_makes_the_read_authoritative() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), IMAGE_CAPABILITIES_V1);
        touch(tmp.path(), "echo-agent-files.occ");
        let caps = load(tmp.path());
        assert!(caps.is_declared());
        assert_eq!(caps.state("echo-agent-files.occ"), Some(true));
        assert!(caps.has("echo-agent-files.occ"));
        // Declared-and-absent, not unknown.
        assert_eq!(caps.state("vercel.cli"), Some(false));
        assert!(!caps.has("vercel.cli"));
        assert_eq!(caps.wire(), ["capabilities.v1", "echo-agent-files.occ"]);
    }

    #[test]
    fn tokens_without_self_token_are_unknown_but_still_wired() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "node.22");
        touch(tmp.path(), "vercel.cli");
        let caps = load(tmp.path());
        assert!(!caps.is_declared());
        assert_eq!(caps.state("node.22"), None);
        assert!(!caps.has("node.22"));
        // The diagnostic content must survive the "unknown" verdict.
        assert_eq!(caps.wire(), ["node.22", "vercel.cli"]);
    }

    #[test]
    fn invalid_names_and_non_files_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), IMAGE_CAPABILITIES_V1);
        touch(tmp.path(), "Grove.Daemon");
        touch(tmp.path(), "README");
        touch(tmp.path(), ".DS_Store");
        touch(tmp.path(), ".file.swp");
        touch(tmp.path(), "notes.txt.");
        touch(
            tmp.path(),
            &format!("a.{}", "b".repeat(MAX_IMAGE_CAPABILITY_LEN)),
        );
        fs::create_dir(tmp.path().join("nested.dir")).unwrap();
        let caps = load(tmp.path());
        assert_eq!(caps.wire(), [IMAGE_CAPABILITIES_V1]);
    }

    #[test]
    fn cap_is_enforced_on_the_sorted_set() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..(MAX_IMAGE_CAPABILITIES + 5) {
            touch(tmp.path(), &format!("cap.t{i:04}"));
        }
        let caps = load(tmp.path());
        assert_eq!(caps.wire().len(), MAX_IMAGE_CAPABILITIES);
        assert_eq!(caps.wire()[0], "cap.t0000");
        assert_eq!(
            caps.wire()[MAX_IMAGE_CAPABILITIES - 1],
            format!("cap.t{:04}", MAX_IMAGE_CAPABILITIES - 1)
        );
    }

    #[test]
    fn overflow_is_unknown_even_when_the_self_token_survives() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), IMAGE_CAPABILITIES_V1);
        // Sorts after `capabilities.v1`, so the self-token is retained.
        for i in 0..MAX_IMAGE_CAPABILITIES {
            touch(tmp.path(), &format!("zz.t{i:04}"));
        }
        let caps = load(tmp.path());
        assert_eq!(caps.wire().len(), MAX_IMAGE_CAPABILITIES);
        assert_eq!(caps.wire()[0], IMAGE_CAPABILITIES_V1);
        // A trimmed set would answer `Some(false)` for tokens that are present.
        assert!(!caps.is_declared());
        assert_eq!(caps.state("zz.t0000"), None);
    }

    #[test]
    fn overflow_can_evict_the_self_token() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), IMAGE_CAPABILITIES_V1);
        for i in 0..MAX_IMAGE_CAPABILITIES {
            touch(tmp.path(), &format!("aa.t{i:04}"));
        }
        let caps = load(tmp.path());
        assert_eq!(caps.wire().len(), MAX_IMAGE_CAPABILITIES);
        assert!(!caps.wire().contains(&IMAGE_CAPABILITIES_V1.to_owned()));
        assert!(!caps.is_declared());
    }

    #[test]
    fn oversized_directory_truncates_the_scan() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), IMAGE_CAPABILITIES_V1);
        for i in 0..MAX_SCANNED_ENTRIES {
            touch(tmp.path(), &format!("ignored{i:05}"));
        }
        let caps = load(tmp.path());
        assert!(!caps.is_declared());
        // A filesystem-order prefix is not reportable as the image's tokens.
        assert!(caps.wire().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_markers_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), IMAGE_CAPABILITIES_V1);
        let target = tmp.path().join("target");
        fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("node.22")).unwrap();
        let caps = load(tmp.path());
        assert_eq!(caps.wire(), [IMAGE_CAPABILITIES_V1]);
    }

    #[test]
    fn dir_override_falls_back_on_blank_values() {
        assert_eq!(capabilities_dir_from_raw(None), DEFAULT_CAPABILITIES_DIR);
        assert_eq!(
            capabilities_dir_from_raw(Some(String::new())),
            DEFAULT_CAPABILITIES_DIR
        );
        assert_eq!(
            capabilities_dir_from_raw(Some("  ".to_owned())),
            DEFAULT_CAPABILITIES_DIR
        );
        assert_eq!(
            capabilities_dir_from_raw(Some("/tmp/caps.d".to_owned())),
            "/tmp/caps.d"
        );
    }

    #[test]
    fn ordering_is_stable_regardless_of_creation_order() {
        let tmp_a = tempfile::tempdir().unwrap();
        for name in ["vercel.cli", IMAGE_CAPABILITIES_V1, "node.22"] {
            touch(tmp_a.path(), name);
        }
        let tmp_b = tempfile::tempdir().unwrap();
        for name in ["node.22", "vercel.cli", IMAGE_CAPABILITIES_V1] {
            touch(tmp_b.path(), name);
        }
        assert_eq!(load(tmp_a.path()).wire(), load(tmp_b.path()).wire());
        assert_eq!(
            load(tmp_a.path()).wire(),
            ["capabilities.v1", "node.22", "vercel.cli"]
        );
    }

    #[test]
    fn only_plausible_tokens_are_noteworthy() {
        for name in [
            "Grove.Daemon",
            "notes.txt",
            "echo_agent_files.occ",
            "trailing.",
        ] {
            assert!(is_rejection_noteworthy(name), "expected warn: {name}");
        }
        for name in [
            "README",
            "notes",
            "file~",
            ".DS_Store",
            ".gitkeep",
            ".file.swp",
        ] {
            assert!(!is_rejection_noteworthy(name), "expected silent: {name}");
        }
    }

    /// The cache is process-wide and reads whatever the host has, so tests may
    /// read it but must assert host-independent invariants only, never a
    /// specific token set. Directory behaviour belongs in `load_from_dir` tests.
    #[test]
    fn cached_accessor_is_internally_consistent() {
        let caps = image_capabilities();
        assert!(caps.wire().len() <= MAX_IMAGE_CAPABILITIES);
        assert_eq!(
            caps.is_declared(),
            caps.state(IMAGE_CAPABILITIES_V1) == Some(true)
        );
    }
}
