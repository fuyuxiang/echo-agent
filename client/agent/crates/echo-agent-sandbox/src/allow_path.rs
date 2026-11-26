//! Normalization of `read_only` / `read_write` sandbox config entries.
//!
//! Security-sensitive parser kept separate from the profile resolver so the
//! policy it implements stays small and easy to audit: allow paths are
//! literal directory grants, and the only rewriting ever performed is
//! stripping one trailing recursive glob down to the directory the user
//! plainly meant. Everything else either passes through byte-for-byte or is
//! rejected — never widened.

use std::path::PathBuf;

use crate::deny::is_glob;

/// Normalize a `read_only` / `read_write` config entry.
///
/// Allow paths are literal directory grants: missing ones are created with
/// `create_dir_all`, then bound. A trailing recursive glob is a common config
/// mistake that used to create a directory literally named `**` and grant
/// access only to it, leaving the intended tree inaccessible — so one trailing
/// `/**`, `/**/`, `/**/*`, or `/*` is stripped to the parent directory (the
/// root forms `/**`, `/**/`, `/**/*`, and `/*` grant `/`, their parent).
///
/// Everything else is taken byte-for-byte from the config, so entries with
/// surrounding whitespace are rejected rather than silently rewritten:
/// trimming would turn `/tmp/* ` (skipped as a glob) into a grant of `/tmp`,
/// and `/srv/cache ` into a grant of a different directory than the one
/// named. Entries still glob-shaped after the single strip ([`is_glob`])
/// cannot be expressed as a directory grant and are skipped with a warning
/// rather than widened. `deny` entries are not affected; globs there are real
/// kernel-enforced patterns.
pub(crate) fn normalize_allow_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    if raw.trim() != raw {
        tracing::warn!(
            path = %raw,
            "sandbox allow path has surrounding whitespace; whitespace is significant in \
             literal paths, so fix the entry; skipping"
        );
        return None;
    }

    let mut s = raw;
    if let Some(parent) = s
        .strip_suffix("/**/*")
        .or_else(|| s.strip_suffix("/**/"))
        .or_else(|| s.strip_suffix("/**"))
        .or_else(|| s.strip_suffix("/*"))
    {
        s = parent.trim_end_matches('/');
        if s.is_empty() {
            // The whole path was a root glob (`/**`, `/**/`, `/**/*`, `/*`):
            // the parent directory is the filesystem root.
            s = "/";
        }
    }

    if is_glob(s) {
        tracing::warn!(
            path = %raw,
            "sandbox read_only/read_write paths are literal directory grants, not globs; \
             name the directory itself (or put globs under `deny`); skipping"
        );
        return None;
    }

    if s != raw {
        tracing::info!(
            original = %raw,
            normalized = %s,
            "stripped trailing glob from sandbox allow path"
        );
    }
    Some(PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_allow_path_cases() {
        for (raw, expected) in [
            // One trailing glob group strips to the parent directory.
            ("/home/u/.cargo/cache/**", Some("/home/u/.cargo/cache")),
            ("/home/u/.cargo/cache/**/", Some("/home/u/.cargo/cache")),
            ("/home/u/.cargo/cache/**/*", Some("/home/u/.cargo/cache")),
            ("/tmp/scratch/*", Some("/tmp/scratch")),
            // Root globs grant the filesystem root, their parent directory.
            ("/**", Some("/")),
            ("/**/", Some("/")),
            ("/**/*", Some("/")),
            ("/*", Some("/")),
            // Literal directory paths pass through unchanged.
            ("/tmp/scratch", Some("/tmp/scratch")),
            ("/tmp/scratch/", Some("/tmp/scratch")),
            ("/", Some("/")),
            // Still glob-shaped after one strip: skip, never widen further
            // (stacked wildcards must not collapse to a higher parent).
            ("/a/**/**", None),
            ("/a/*/*", None),
            ("/home/**/cache", None),
            ("/tmp/foo*", None),
            // Surrounding whitespace is rejected, never trimmed: trimming
            // would widen `/tmp/* ` into a grant of /tmp and rewrite
            // `/srv/cache ` into a different directory than configured.
            ("/tmp/* ", None),
            ("/srv/cache ", None),
            (" /srv/cache", None),
            ("   ", None),
            // Nothing configured, nothing to grant.
            ("", None),
        ] {
            assert_eq!(
                normalize_allow_path(raw),
                expected.map(PathBuf::from),
                "normalize_allow_path({raw:?})"
            );
        }
    }
}
