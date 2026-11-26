//! Built-in files extracted to `~/.echo-agent/` on startup.

const BUILTIN_FILES: &[(&str, &str)] = &[("README.md", include_str!("../README.md"))];

/// Extract built-in metadata files to `~/.echo-agent/` on startup.
///
/// User skills under `~/.echo-agent/skills/` are never managed here. Platform skills
/// are delivered separately through the bundled skill cache.
pub fn extract_builtin_files(echo_agent_home: &std::path::Path) {
    let version = echo_agent_version::VERSION;
    let marker = echo_agent_home.join(".metadata_version");

    if let Ok(existing) = std::fs::read_to_string(&marker)
        && existing.trim() == version
    {
        return;
    }

    let _ = std::fs::create_dir_all(echo_agent_home);

    // Clean up cached changelog files from previous version so
    // /release-notes fetches fresh content for the new version.
    for stale in &["CHANGELOG.json", "CHANGELOG.md"] {
        let _ = std::fs::remove_file(echo_agent_home.join(stale));
    }

    for &(filename, content) in BUILTIN_FILES {
        if let Err(e) = std::fs::write(echo_agent_home.join(filename), content) {
            tracing::debug!(error = %e, filename, "Failed to extract built-in file");
        }
    }

    let _ = std::fs::write(&marker, version);
    tracing::debug!(version, "Extracted built-in files");
}

/// `(name, sha256)` of every `SKILL.md` body ever extracted into
/// `$ECHO_AGENT_HOME/skills/`; `help` rows hash the pre-substitution bytes.
const FORMER_PLATFORM_SKILL_HASHES: &[(&str, &str)] = &[
    (
        "create-skill",
        "bee1d45089c6374a3349b2db7ed1b9e209878ebe031d651f410212746173bd81",
    ),
    (
        "create-skill",
        "e383b0013d4b595c141de64b111027f443bf6375c0e8ba1b90c23cd118b2ffd3",
    ),
    (
        "create-skill",
        "fead9243b902177003c6f0a9d51c6768e20ddcb388be15d68769950d440bfd61",
    ),
    (
        "code-review",
        "df6f708a5207f34e1d8b7775982788406d7c46e722d50aedde2d0300f420a10a",
    ),
    (
        "imagine",
        "1754ff52d1d043841fde3cd9ed0f715315b8f06dc18bf2995be59bcde5960def",
    ),
    (
        "imagine",
        "34f024b6636495bf1d453072fde41fd7cb8726d57436c8b1d4773fbe1c112a56",
    ),
    (
        "imagine",
        "ced79ca46b183e21c3ef484f199870c265d0a6c5f3a4f25470079816af2bcb20",
    ),
    (
        "create-workflow",
        "51345342753d1c7b0e8d3a671458df4a3bc0b1c94e683ab5dc1c3c97c2bf652e",
    ),
    (
        "help",
        "f1e921dbd64725e801d25f2b8879dd49490ff47a7e64a23fb7ac6f765f726b4d",
    ),
    (
        "help",
        "cdd34d84b40f2fdccb74893939751e2ec9ef2463149ec8d001840f151e317491",
    ),
    (
        "help",
        "7c507f7095b2b80188a994de476012cfb876f186aadab959ed546df0c0f6a42d",
    ),
    (
        "help",
        "4595c5fc67cb8d9159f45b18516a6aa4fa092cd5e5cd996688c4faa2dfb17c62",
    ),
    (
        "help",
        "2811d78c10d4983c0b1083579fc6ae7cc399d70b67ff4f2efc3ca98fd882ca70",
    ),
    (
        "check-work",
        "1d9044a4f02c3abcb4153783d451611731f669643ba371bb6069a1e1c2d3e95d",
    ),
    (
        "check-work",
        "fcc1b642413af11e8077c3eda96f412a1d3894c2ef8ee06b597012b9e912f213",
    ),
    (
        "check-work",
        "51a57345db2b6c393bcd83c635b586bf4e821e680ae96c4f7b2d34c43ea00582",
    ),
    (
        "best-of-n",
        "e0e1849ead8d884030e1d34bb651517c9961918f6e1ec62457c68dd2be06636b",
    ),
    (
        "check",
        "5ddcd2f42eaeb6efc27dc1a651ad36be71e331e58da8ed16ec7a28b2f8bb0d4e",
    ),
    (
        "check",
        "feeab8afc3040071229e0ff0803795f25b180dd1f8f8e697caa09953fcffc300",
    ),
    (
        "docx",
        "cfbabd72b1aec7dfaad988fb6e5e16b27dc744b9a00cae15db9045e95f53903e",
    ),
    (
        "pptx",
        "e5b0df918cbe9d62508b3cf808d73899633f82fc19f6258a25ef9f0de7e62125",
    ),
    (
        "xlsx",
        "dd316db7785a6be12966c2a2d848931fbf5f518b3cdb0a66fa62ee835ead1cfc",
    ),
];

/// Remove platform-skill leftovers extracted into `$ECHO_AGENT_HOME/skills/` by
/// pre-bundle binaries, where they shadow `bundled/skills/`. Only dirs whose
/// `SKILL.md` byte-matches a known shipped body are removed; user skills and
/// edits are kept. Runs every startup so restored backups get re-cleaned.
pub fn purge_stale_extracted_skills(echo_agent_home: &std::path::Path) {
    purge_skill_dirs_matching(echo_agent_home, FORMER_PLATFORM_SKILL_HASHES);
}

fn purge_skill_dirs_matching(echo_agent_home: &std::path::Path, known: &[(&str, &str)]) {
    // For reversing the extract-time `~/.echo-agent/` -> home rewrite (help only).
    let home_prefix = format!("{}/", echo_agent_home.to_string_lossy());

    let mut names: Vec<&str> = known.iter().map(|&(name, _)| name).collect();
    names.dedup();

    for name in names {
        let dir = echo_agent_home.join("skills").join(name);
        // Absent or unreadable: nothing provably ours.
        let Ok(content) = std::fs::read_to_string(dir.join("SKILL.md")) else {
            continue;
        };
        let on_disk = sha256_hex(content.as_bytes());
        let unrewritten = sha256_hex(content.replace(&home_prefix, "~/.echo-agent/").as_bytes());
        let managed = known
            .iter()
            .any(|&(n, hash)| n == name && (hash == on_disk || hash == unrewritten));
        if !managed {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                tracing::info!(name, "removed stale extracted platform skill");
            }
            Err(e) => {
                tracing::warn!(name, error = %e, "failed to remove stale extracted platform skill");
            }
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_bump_reextracts_metadata_without_touching_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        extract_builtin_files(home);
        std::fs::write(home.join("README.md"), "old").unwrap();
        std::fs::write(home.join(".metadata_version"), "0.0.0-stale").unwrap();

        let skill_names = [
            "help",
            "create-skill",
            "code-review",
            "imagine",
            "check-work",
            "check",
            "best-of-n",
            "docx",
            "pptx",
            "xlsx",
        ];
        for name in skill_names {
            let dir = home.join("skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("custom {name}")).unwrap();
            std::fs::write(dir.join("user-file.txt"), "keep").unwrap();
        }

        extract_builtin_files(home);

        assert_ne!(
            std::fs::read_to_string(home.join("README.md")).unwrap(),
            "old"
        );
        for name in skill_names {
            let dir = home.join("skills").join(name);
            assert_eq!(
                std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
                format!("custom {name}")
            );
            assert_eq!(
                std::fs::read_to_string(dir.join("user-file.txt")).unwrap(),
                "keep"
            );
        }
    }

    #[test]
    fn same_version_does_not_restore_missing_or_delete_legacy_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("skills/check")).unwrap();
        std::fs::write(home.join("skills/check/SKILL.md"), "custom check").unwrap();
        std::fs::write(home.join(".metadata_version"), echo_agent_version::VERSION).unwrap();

        extract_builtin_files(home);

        assert!(!home.join("skills/help/SKILL.md").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("skills/check/SKILL.md")).unwrap(),
            "custom check"
        );
    }

    const EXTRACTED_BODY: &str = "old platform skill body\n";
    const OLDER_EXTRACTED_BODY: &str = "even older platform skill body\n";

    /// Purge against a table with two create-skill generations and one help body.
    fn purge_with_test_table(home: &std::path::Path) {
        let current = sha256_hex(EXTRACTED_BODY.as_bytes());
        let older = sha256_hex(OLDER_EXTRACTED_BODY.as_bytes());
        purge_skill_dirs_matching(
            home,
            &[
                ("create-skill", current.as_str()),
                ("create-skill", older.as_str()),
                ("help", current.as_str()),
            ],
        );
    }

    fn write_skill(home: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let dir = home.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
        dir
    }

    #[test]
    fn purge_removes_any_managed_generation_with_companions() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = write_skill(home, "create-skill", OLDER_EXTRACTED_BODY);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/gen.py"), "print()").unwrap();

        purge_with_test_table(home);

        assert!(!dir.exists());
    }

    #[test]
    fn purge_fails_closed_on_anything_not_provably_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Missing skills/ is a no-op and must not create it.
        purge_with_test_table(home);
        assert!(!home.join("skills").exists());

        let edited = write_skill(home, "create-skill", "my customized body\n");
        // Managed bytes under a name we never extracted.
        let unknown = write_skill(home, "my-custom", EXTRACTED_BODY);
        // Former platform name without a SKILL.md: unknown layout.
        let no_manifest = home.join("skills/help");
        std::fs::create_dir_all(&no_manifest).unwrap();
        std::fs::write(no_manifest.join("notes.txt"), "user data").unwrap();

        purge_with_test_table(home);

        assert!(edited.join("SKILL.md").exists());
        assert!(unknown.join("SKILL.md").exists());
        assert!(no_manifest.join("notes.txt").exists());
    }

    #[test]
    fn purge_reverses_home_rewrite_before_hashing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // help's on-disk bytes are machine-dependent (home substituted in).
        let raw = EXTRACTED_BODY.replace(
            "platform",
            "read ~/.echo-agent/docs/user-guide then platform",
        );
        let body = raw.replace("~/.echo-agent/", &format!("{}/", home.to_string_lossy()));
        let raw_hash = sha256_hex(raw.as_bytes());
        let dir = write_skill(home, "help", &body);

        purge_skill_dirs_matching(home, &[("help", raw_hash.as_str())]);

        assert!(!dir.exists());
    }

    #[test]
    fn shipped_hash_table_is_well_formed() {
        let mut names: Vec<&str> = FORMER_PLATFORM_SKILL_HASHES
            .iter()
            .map(|&(n, _)| n)
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names,
            [
                "best-of-n",
                "check",
                "check-work",
                "code-review",
                "create-skill",
                "create-workflow",
                "docx",
                "help",
                "imagine",
                "pptx",
                "xlsx",
            ]
        );
        for &(_, hash) in FORMER_PLATFORM_SKILL_HASHES {
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
}
