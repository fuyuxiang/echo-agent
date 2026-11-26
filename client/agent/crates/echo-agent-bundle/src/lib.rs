//! On-disk cache for the EchoAgent-published subagent bundle: personas, roles,
//! agents, skills, and workflows written under `<echoagent home>/bundled`.
//!
//! Writes are checksum-tracked through `manifest.json`, so a file the user
//! edited by hand is never overwritten and never pruned. Archive extraction
//! is bounded (entry count, per-entry size, total decompressed size) and every
//! path is sanitized before it is joined onto the cache root.

use anyhow::{Context, Result, bail};
use echo_agent_chat_proxy_types::SubagentBundle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

const BUNDLED_DIR_NAME: &str = "bundled";
const MANIFEST_FILE_NAME: &str = "manifest.json";

const ARCHIVE_MAX_DECOMPRESSED_SIZE: usize = 50 * 1024 * 1024;
const ARCHIVE_MAX_ENTRIES: usize = 1000;
const ARCHIVE_MAX_ENTRY_SIZE: u64 = 1024 * 1024;

#[derive(Deserialize)]
struct ArchiveBundleMetadata {
    version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub version: String,
    pub checksums: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundleFileKind {
    Persona,
    Role,
    Agent,
    Skill,
    Workflow,
}

impl BundleFileKind {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Persona => "personas",
            Self::Role => "roles",
            Self::Agent => "agents",
            Self::Skill => "skills",
            Self::Workflow => "workflows",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Agent | Self::Skill => "md",
            Self::Persona | Self::Role => "toml",
            Self::Workflow => "rhai",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Role => "role",
            Self::Agent => "agent",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
        }
    }

    fn from_dir_name(dir_name: &str) -> Option<Self> {
        match dir_name {
            "personas" => Some(Self::Persona),
            "roles" => Some(Self::Role),
            "agents" => Some(Self::Agent),
            "skills" => Some(Self::Skill),
            "workflows" => Some(Self::Workflow),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundleFileState {
    Absent,
    MatchesManaged,
    ModifiedOrUnmanaged,
}

#[derive(Debug)]
struct BundleFile<'a> {
    relative_path: String,
    checksum: String,
    content: &'a str,
}

pub fn bundled_root() -> PathBuf {
    echo_agent_config::echo_agent_home().join(BUNDLED_DIR_NAME)
}

pub fn read_cached_manifest(root: &Path) -> Result<Option<BundleManifest>> {
    let manifest_path = manifest_path(root);
    let bytes = match std::fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", manifest_path.display()));
        }
    };

    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))
        .map(Some)
}

pub fn write_bundle_to_cache(root: &Path, bundle: &SubagentBundle) -> Result<BundleManifest> {
    let old_manifest = read_cached_manifest(root)?.map(sanitize_manifest);
    ensure_bundle_dirs(root)?;

    let bundle_files = bundle_files(bundle)?;
    let mut next_checksums = HashMap::new();

    for bundle_file in &bundle_files {
        let previous_checksum = old_manifest
            .as_ref()
            .and_then(|manifest| manifest.checksums.get(&bundle_file.relative_path));
        let absolute_path = root.join(&bundle_file.relative_path);

        match bundle_file_state(&absolute_path, previous_checksum.map(String::as_str))? {
            BundleFileState::Absent | BundleFileState::MatchesManaged => {
                write_bundle_file(&absolute_path, bundle_file.content.as_bytes())?;
                next_checksums.insert(
                    bundle_file.relative_path.clone(),
                    bundle_file.checksum.clone(),
                );
            }
            BundleFileState::ModifiedOrUnmanaged => {
                if let Some(previous_checksum) = previous_checksum {
                    next_checksums
                        .insert(bundle_file.relative_path.clone(), previous_checksum.clone());
                }
            }
        }
    }

    // JSON payload has no workflows field. Keep managed workflows from the
    // last archive extract so a JSON fallback does not delete them.
    if let Some(old_manifest) = old_manifest.as_ref() {
        for (path, checksum) in &old_manifest.checksums {
            if path.starts_with("workflows/") {
                next_checksums
                    .entry(path.clone())
                    .or_insert_with(|| checksum.clone());
            }
        }
    }

    if let Some(old_manifest) = old_manifest.as_ref() {
        prune_removed_files(root, old_manifest, &mut next_checksums)?;
    }

    let next_manifest = BundleManifest {
        version: bundle.version.clone(),
        checksums: next_checksums,
    };
    let manifest_json =
        serde_json::to_vec_pretty(&next_manifest).context("failed to serialize bundle manifest")?;
    std::fs::write(manifest_path(root), manifest_json)
        .with_context(|| format!("failed to write {}", manifest_path(root).display()))?;

    Ok(next_manifest)
}

pub fn extract_bundle_archive(root: &Path, archive_bytes: &[u8]) -> Result<BundleManifest> {
    let decoder = flate2::read::GzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(decoder);

    let old_manifest = read_cached_manifest(root)?.map(sanitize_manifest);
    ensure_bundle_dirs(root)?;

    let mut next_checksums = HashMap::new();
    let mut version = String::new();
    let mut total_decompressed: usize = 0;
    let mut entry_count: usize = 0;

    for entry_result in archive
        .entries()
        .context("failed to read archive entries")?
    {
        let mut entry = entry_result.context("failed to read archive entry")?;

        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }

        entry_count += 1;
        if entry_count > ARCHIVE_MAX_ENTRIES {
            bail!("archive exceeds maximum entry count ({ARCHIVE_MAX_ENTRIES})");
        }

        let entry_size = entry.header().size().context("failed to read entry size")?;
        if entry_size > ARCHIVE_MAX_ENTRY_SIZE {
            bail!("archive entry exceeds maximum size ({ARCHIVE_MAX_ENTRY_SIZE} bytes)");
        }

        total_decompressed = total_decompressed
            .checked_add(entry_size as usize)
            .context("decompressed size overflow")?;
        if total_decompressed > ARCHIVE_MAX_DECOMPRESSED_SIZE {
            bail!(
                "archive exceeds maximum decompressed size ({ARCHIVE_MAX_DECOMPRESSED_SIZE} bytes)"
            );
        }

        let raw_path = entry
            .path()
            .context("failed to read entry path")?
            .to_string_lossy()
            .into_owned();
        let path = raw_path.strip_prefix("./").unwrap_or(&raw_path);

        if path == "bundle.json" {
            let mut content = String::new();
            entry
                .read_to_string(&mut content)
                .context("failed to read bundle.json")?;
            let meta: ArchiveBundleMetadata =
                serde_json::from_str(&content).context("failed to parse bundle.json")?;
            version = meta.version;
            continue;
        }

        let cache_relative_path = match map_archive_path_to_cache_path(path) {
            Some(p) => p,
            None => continue,
        };

        let mut content = Vec::with_capacity(entry_size as usize);
        entry
            .read_to_end(&mut content)
            .with_context(|| format!("failed to read archive entry: {path}"))?;
        let checksum = checksum_bytes(&content);

        let absolute_path = root.join(&cache_relative_path);
        let previous_checksum = old_manifest
            .as_ref()
            .and_then(|m| m.checksums.get(&cache_relative_path));

        match bundle_file_state(&absolute_path, previous_checksum.map(String::as_str))? {
            BundleFileState::Absent | BundleFileState::MatchesManaged => {
                write_bundle_file(&absolute_path, &content)?;
                next_checksums.insert(cache_relative_path, checksum);
            }
            BundleFileState::ModifiedOrUnmanaged => {
                if let Some(prev) = previous_checksum {
                    next_checksums.insert(cache_relative_path, prev.clone());
                }
            }
        }
    }

    if version.is_empty() {
        bail!("archive missing bundle.json with version field");
    }

    if let Some(old_manifest) = old_manifest.as_ref() {
        prune_removed_files(root, old_manifest, &mut next_checksums)?;
    }

    let next_manifest = BundleManifest {
        version,
        checksums: next_checksums,
    };
    let manifest_json =
        serde_json::to_vec_pretty(&next_manifest).context("failed to serialize bundle manifest")?;
    std::fs::write(manifest_path(root), manifest_json)
        .with_context(|| format!("failed to write {}", manifest_path(root).display()))?;

    Ok(next_manifest)
}

fn checksum_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn checksum_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read {} for checksum", path.display()))?;
    Ok(checksum_bytes(&bytes))
}

/// True when `relative_path` is in the bundle manifest and the on-disk bytes
/// still match that checksum (not a local/agent overwrite).
pub fn is_managed_bundle_file(root: &Path, relative_path: &str) -> bool {
    let relative_path = relative_path.replace('\\', "/");
    let Ok(Some(manifest)) = read_cached_manifest(root) else {
        return false;
    };
    let Some(expected) = manifest.checksums.get(&relative_path) else {
        return false;
    };
    matches!(
        bundle_file_state(&root.join(&relative_path), Some(expected.as_str())),
        Ok(BundleFileState::MatchesManaged)
    )
}

fn prune_removed_files(
    root: &Path,
    old_manifest: &BundleManifest,
    retained_checksums: &mut HashMap<String, String>,
) -> Result<()> {
    for (relative_path, previous_checksum) in sanitize_manifest(old_manifest.clone()).checksums {
        if retained_checksums.contains_key(&relative_path) {
            continue;
        }

        let absolute_path = root.join(&relative_path);
        match bundle_file_state(&absolute_path, Some(previous_checksum.as_str()))? {
            BundleFileState::Absent => {}
            BundleFileState::MatchesManaged => {
                std::fs::remove_file(&absolute_path)
                    .with_context(|| format!("failed to remove {}", absolute_path.display()))?;
            }
            BundleFileState::ModifiedOrUnmanaged => {
                retained_checksums.insert(relative_path, previous_checksum);
            }
        }
    }

    Ok(())
}

fn ensure_bundle_dirs(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create {}", root.display()))?;

    for dir_name in ["personas", "roles", "agents", "skills", "workflows"] {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }

    Ok(())
}

fn manifest_path(root: &Path) -> PathBuf {
    root.join(MANIFEST_FILE_NAME)
}

fn checksum_file_if_exists(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(checksum_bytes(&bytes))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed to read {} for checksum", path.display()))
        }
    }
}

fn write_bundle_file(absolute_path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = absolute_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(absolute_path, content)
        .with_context(|| format!("failed to write {}", absolute_path.display()))?;
    Ok(())
}

fn bundle_file_state(path: &Path, old_checksum: Option<&str>) -> Result<BundleFileState> {
    let current_checksum = match checksum_file_if_exists(path)? {
        Some(checksum) => checksum,
        None => return Ok(BundleFileState::Absent),
    };

    Ok(match old_checksum {
        Some(old_checksum) if current_checksum == old_checksum => BundleFileState::MatchesManaged,
        Some(_) | None => BundleFileState::ModifiedOrUnmanaged,
    })
}

fn sanitize_manifest(manifest: BundleManifest) -> BundleManifest {
    let checksums = manifest
        .checksums
        .into_iter()
        .filter_map(|(relative_path, checksum)| {
            sanitize_relative_path(&relative_path).map(|relative_path| (relative_path, checksum))
        })
        .collect();

    BundleManifest {
        version: manifest.version,
        checksums,
    }
}

fn sanitize_relative_path(relative_path: &str) -> Option<String> {
    if relative_path.is_empty() || relative_path.starts_with('/') || relative_path.contains('\\') {
        return None;
    }

    let mut parts = relative_path.split('/');
    let dir_name = parts.next()?;
    let second = parts.next()?;

    match parts.next() {
        None => {
            if second.is_empty() {
                return None;
            }
            let kind = BundleFileKind::from_dir_name(dir_name)?;
            if kind == BundleFileKind::Skill {
                return None;
            }
            let file_stem = second.strip_suffix(&format!(".{}", kind.extension()))?;
            validate_bundle_name(kind, file_stem).ok()?;
            Some(relative_path_for(kind, file_stem))
        }
        Some(third) => {
            if dir_name != "skills" {
                return None;
            }
            validate_bundle_name(BundleFileKind::Skill, second).ok()?;
            // Reject components that would let extraction escape the per-skill directory.
            for component in std::iter::once(third).chain(parts) {
                if component.is_empty()
                    || component == "."
                    || component == ".."
                    || component.chars().any(char::is_control)
                {
                    return None;
                }
            }
            Some(relative_path.to_string())
        }
    }
}

fn map_archive_path_to_cache_path(archive_path: &str) -> Option<String> {
    if let Some(rest) = archive_path.strip_prefix("subagents/") {
        return sanitize_relative_path(rest);
    }
    if archive_path.starts_with("skills/") {
        return sanitize_relative_path(archive_path);
    }
    if archive_path.starts_with("workflows/") {
        return sanitize_relative_path(archive_path);
    }
    None
}

pub fn count_entries_by_prefix(manifest: &BundleManifest, prefix: &str) -> usize {
    manifest
        .checksums
        .keys()
        .filter(|k| k.starts_with(prefix))
        .count()
}

fn bundle_files(bundle: &SubagentBundle) -> Result<Vec<BundleFile<'_>>> {
    let mut files = Vec::new();
    extend_bundle_files(&mut files, BundleFileKind::Persona, &bundle.personas)?;
    extend_bundle_files(&mut files, BundleFileKind::Role, &bundle.roles)?;
    extend_bundle_files(&mut files, BundleFileKind::Agent, &bundle.agents)?;
    extend_bundle_files(&mut files, BundleFileKind::Skill, &bundle.skills)?;
    Ok(files)
}

fn extend_bundle_files<'a>(
    files: &mut Vec<BundleFile<'a>>,
    kind: BundleFileKind,
    entries: &'a HashMap<String, String>,
) -> Result<()> {
    for (name, content) in entries {
        validate_bundle_name(kind, name)?;
        files.push(BundleFile {
            relative_path: relative_path_for(kind, name),
            checksum: checksum_bytes(content.as_bytes()),
            content,
        });
    }
    Ok(())
}

fn relative_path_for(kind: BundleFileKind, name: &str) -> String {
    match kind {
        BundleFileKind::Skill => format!("{}/{name}/SKILL.md", kind.dir_name()),
        _ => format!("{}/{name}.{}", kind.dir_name(), kind.extension()),
    }
}

fn validate_bundle_name(kind: BundleFileKind, name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        bail!("invalid bundled {} name: {name:?}", kind.label());
    }

    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_helpers {
    pub fn make_test_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for &(path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, content).unwrap();
        }
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    pub fn bundle_json(version: &str) -> String {
        format!(r#"{{"version":"{version}"}}"#)
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{bundle_json, make_test_archive};
    use super::*;
    use tempfile::TempDir;

    fn bundle_with_persona(version: &str, name: &str, content: &str) -> SubagentBundle {
        let mut bundle = SubagentBundle::empty(version);
        bundle
            .personas
            .insert(name.to_string(), content.to_string());
        bundle
    }

    fn bundle_with_skill(version: &str, name: &str, content: &str) -> SubagentBundle {
        let mut bundle = SubagentBundle::empty(version);
        bundle.skills.insert(name.to_string(), content.to_string());
        bundle
    }

    fn cache_root(tmp: &TempDir) -> PathBuf {
        tmp.path().join("bundled")
    }

    #[test]
    fn write_new_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let bundle = bundle_with_persona("v1", "researcher", "instructions = \"hello\"");

        let manifest = write_bundle_to_cache(&root, &bundle).unwrap();

        assert_eq!(manifest.version, "v1");
        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"hello\""
        );
        assert_eq!(read_cached_manifest(&root).unwrap(), Some(manifest));
    }

    #[test]
    fn json_fallback_does_not_prune_managed_workflows() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let archive = make_test_archive(&[
            ("bundle.json", v.as_bytes()),
            (
                "workflows/deep-research.rhai",
                b"let meta = #{ name: \"deep-research\", description: \"d\" };",
            ),
        ]);
        extract_bundle_archive(&root, &archive).unwrap();
        assert!(root.join("workflows/deep-research.rhai").is_file());

        let manifest = write_bundle_to_cache(&root, &SubagentBundle::empty("v2")).unwrap();
        assert!(root.join("workflows/deep-research.rhai").is_file());
        assert!(
            manifest
                .checksums
                .contains_key("workflows/deep-research.rhai")
        );
    }

    #[test]
    fn overwrite_unchanged_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v1", "researcher", "instructions = \"old\""),
        )
        .unwrap();

        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v2", "researcher", "instructions = \"new\""),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"new\""
        );
    }

    #[test]
    fn skip_user_modified_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let manifest_v1 = write_bundle_to_cache(
            &root,
            &bundle_with_persona("v1", "researcher", "instructions = \"old\""),
        )
        .unwrap();
        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"user edit\"",
        )
        .unwrap();

        let manifest_v2 = write_bundle_to_cache(
            &root,
            &bundle_with_persona("v2", "researcher", "instructions = \"new\""),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"user edit\""
        );
        assert_eq!(
            manifest_v2.checksums.get("personas/researcher.toml"),
            manifest_v1.checksums.get("personas/researcher.toml")
        );
    }

    #[test]
    fn prune_removed_unmodified_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v1", "researcher", "instructions = \"old\""),
        )
        .unwrap();

        let manifest = write_bundle_to_cache(&root, &SubagentBundle::empty("v2")).unwrap();

        assert!(!root.join("personas/researcher.toml").exists());
        assert!(!manifest.checksums.contains_key("personas/researcher.toml"));
    }

    #[test]
    fn keep_removed_modified_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let manifest_v1 = write_bundle_to_cache(
            &root,
            &bundle_with_persona("v1", "researcher", "instructions = \"old\""),
        )
        .unwrap();
        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"user edit\"",
        )
        .unwrap();

        let manifest_v2 = write_bundle_to_cache(&root, &SubagentBundle::empty("v2")).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"user edit\""
        );
        assert_eq!(
            manifest_v2.checksums.get("personas/researcher.toml"),
            manifest_v1.checksums.get("personas/researcher.toml")
        );
    }

    #[test]
    fn write_rejects_path_traversal_names() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let outside = tmp.path().join("outside.toml");
        let bundle = bundle_with_persona("v1", "../../outside", "instructions = \"evil\"");

        let error = write_bundle_to_cache(&root, &bundle).unwrap_err();

        assert!(error.to_string().contains("invalid bundled persona name"));
        assert!(!outside.exists());
    }

    #[test]
    fn prune_skips_unsafe_manifest_paths() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        std::fs::create_dir_all(&root).unwrap();

        let outside = tmp.path().join("outside.toml");
        std::fs::write(&outside, "keep me").unwrap();

        let old_manifest = BundleManifest {
            version: "v1".to_string(),
            checksums: HashMap::from([(
                "personas/../../outside.toml".to_string(),
                checksum_file(&outside).unwrap(),
            )]),
        };
        let mut retained = HashMap::new();

        prune_removed_files(&root, &old_manifest, &mut retained).unwrap();

        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me");
        assert!(retained.is_empty());
    }

    #[test]
    fn user_revert_after_skipped_update_allows_future_update() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v1", "researcher", "instructions = \"old\""),
        )
        .unwrap();
        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"user edit\"",
        )
        .unwrap();
        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v2", "researcher", "instructions = \"new\""),
        )
        .unwrap();

        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"old\"",
        )
        .unwrap();

        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v3", "researcher", "instructions = \"latest\""),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"latest\""
        );
    }

    #[test]
    fn user_revert_after_preserved_remove_allows_future_prune() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        write_bundle_to_cache(
            &root,
            &bundle_with_persona("v1", "researcher", "instructions = \"old\""),
        )
        .unwrap();
        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"user edit\"",
        )
        .unwrap();
        write_bundle_to_cache(&root, &SubagentBundle::empty("v2")).unwrap();

        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"old\"",
        )
        .unwrap();

        let manifest = write_bundle_to_cache(&root, &SubagentBundle::empty("v3")).unwrap();

        assert!(!root.join("personas/researcher.toml").exists());
        assert!(!manifest.checksums.contains_key("personas/researcher.toml"));
    }

    #[test]
    fn same_version_retry_repairs_missing_managed_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let bundle = bundle_with_persona("v1", "researcher", "instructions = \"hello\"");

        write_bundle_to_cache(&root, &bundle).unwrap();
        std::fs::remove_file(root.join("personas/researcher.toml")).unwrap();

        write_bundle_to_cache(&root, &bundle).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"hello\""
        );
    }

    #[test]
    fn write_new_skill_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let bundle = bundle_with_skill("v1", "commit", "# Commit Skill\nRun git commit.");

        let manifest = write_bundle_to_cache(&root, &bundle).unwrap();

        assert_eq!(manifest.version, "v1");
        assert_eq!(
            std::fs::read_to_string(root.join("skills/commit/SKILL.md")).unwrap(),
            "# Commit Skill\nRun git commit."
        );
        assert!(manifest.checksums.contains_key("skills/commit/SKILL.md"));
    }

    #[test]
    fn skip_user_modified_skill() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let manifest_v1 =
            write_bundle_to_cache(&root, &bundle_with_skill("v1", "commit", "# Original")).unwrap();
        std::fs::write(root.join("skills/commit/SKILL.md"), "# User custom").unwrap();

        let manifest_v2 =
            write_bundle_to_cache(&root, &bundle_with_skill("v2", "commit", "# Updated")).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("skills/commit/SKILL.md")).unwrap(),
            "# User custom"
        );
        assert_eq!(
            manifest_v2.checksums.get("skills/commit/SKILL.md"),
            manifest_v1.checksums.get("skills/commit/SKILL.md")
        );
    }

    #[test]
    fn prune_removed_unmodified_skill() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        write_bundle_to_cache(&root, &bundle_with_skill("v1", "commit", "# Original")).unwrap();

        let manifest = write_bundle_to_cache(&root, &SubagentBundle::empty("v2")).unwrap();

        assert!(!root.join("skills/commit/SKILL.md").exists());
        assert!(!manifest.checksums.contains_key("skills/commit/SKILL.md"));
    }

    #[test]
    fn keep_removed_modified_skill() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let manifest_v1 =
            write_bundle_to_cache(&root, &bundle_with_skill("v1", "commit", "# Original")).unwrap();
        std::fs::write(root.join("skills/commit/SKILL.md"), "# User custom").unwrap();

        let manifest_v2 = write_bundle_to_cache(&root, &SubagentBundle::empty("v2")).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("skills/commit/SKILL.md")).unwrap(),
            "# User custom"
        );
        assert_eq!(
            manifest_v2.checksums.get("skills/commit/SKILL.md"),
            manifest_v1.checksums.get("skills/commit/SKILL.md")
        );
    }

    #[test]
    fn skill_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let outside = tmp.path().join("outside.md");
        let bundle = bundle_with_skill("v1", "../../outside", "# evil");

        let error = write_bundle_to_cache(&root, &bundle).unwrap_err();

        assert!(error.to_string().contains("invalid bundled skill name"));
        assert!(!outside.exists());
    }

    #[test]
    fn sanitize_accepts_valid_skill_path() {
        assert_eq!(
            sanitize_relative_path("skills/commit/SKILL.md"),
            Some("skills/commit/SKILL.md".to_string())
        );
    }

    #[test]
    fn sanitize_accepts_nested_skill_paths() {
        assert_eq!(
            sanitize_relative_path("skills/implement/scripts/memory.py"),
            Some("skills/implement/scripts/memory.py".to_string())
        );
        assert_eq!(
            sanitize_relative_path("skills/implement/tests/test_memory.py"),
            Some("skills/implement/tests/test_memory.py".to_string())
        );
        assert_eq!(
            sanitize_relative_path("skills/commit/README.md"),
            Some("skills/commit/README.md".to_string())
        );
        assert_eq!(
            sanitize_relative_path("skills/foo/a/b/c/d.txt"),
            Some("skills/foo/a/b/c/d.txt".to_string())
        );
    }

    #[test]
    fn sanitize_rejects_invalid_skill_paths() {
        // Wrong top-level directory.
        assert_eq!(sanitize_relative_path("personas/commit/SKILL.md"), None);
        // Two-component skill path (must be at least 3).
        assert_eq!(sanitize_relative_path("skills/commit.md"), None);
        // Empty skill name.
        assert_eq!(sanitize_relative_path("skills//SKILL.md"), None);
        // Path traversal in skill name.
        assert_eq!(sanitize_relative_path("skills/../SKILL.md"), None);
        assert_eq!(sanitize_relative_path("skills/../etc/SKILL.md"), None);
        // Path traversal in nested components.
        assert_eq!(sanitize_relative_path("skills/foo/../bar/SKILL.md"), None);
        assert_eq!(
            sanitize_relative_path("skills/foo/scripts/../../etc/passwd"),
            None
        );
        // `.` and empty components in the nested portion are rejected.
        assert_eq!(sanitize_relative_path("skills/foo/./SKILL.md"), None);
        assert_eq!(sanitize_relative_path("skills/foo//SKILL.md"), None);
    }

    #[test]
    fn sanitize_accepts_valid_two_component_paths() {
        assert_eq!(
            sanitize_relative_path("personas/researcher.toml"),
            Some("personas/researcher.toml".to_string())
        );
        assert_eq!(
            sanitize_relative_path("roles/reviewer.toml"),
            Some("roles/reviewer.toml".to_string())
        );
        assert_eq!(
            sanitize_relative_path("agents/coder.md"),
            Some("agents/coder.md".to_string())
        );
    }

    // --- archive extraction tests ---

    #[test]
    fn extract_archive_writes_personas_roles_agents_and_skills() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let archive = make_test_archive(&[
            ("bundle.json", v.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"hello\"",
            ),
            ("subagents/roles/reviewer.toml", b"description = \"review\""),
            ("subagents/agents/default.md", b"# agent"),
            ("skills/commit/SKILL.md", b"# Commit skill"),
            (
                "workflows/deep-research.rhai",
                b"let meta = #{ name: \"deep-research\", description: \"d\" };",
            ),
        ]);

        let manifest = extract_bundle_archive(&root, &archive).unwrap();

        assert_eq!(manifest.version, "v1");
        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"hello\""
        );
        assert_eq!(
            std::fs::read_to_string(root.join("roles/reviewer.toml")).unwrap(),
            "description = \"review\""
        );
        assert_eq!(
            std::fs::read_to_string(root.join("agents/default.md")).unwrap(),
            "# agent"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("skills/commit/SKILL.md")).unwrap(),
            "# Commit skill"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("workflows/deep-research.rhai")).unwrap(),
            "let meta = #{ name: \"deep-research\", description: \"d\" };"
        );
        assert!(manifest.checksums.contains_key("personas/researcher.toml"));
        assert!(manifest.checksums.contains_key("roles/reviewer.toml"));
        assert!(manifest.checksums.contains_key("agents/default.md"));
        assert!(manifest.checksums.contains_key("skills/commit/SKILL.md"));
        assert!(
            manifest
                .checksums
                .contains_key("workflows/deep-research.rhai")
        );
        assert!(is_managed_bundle_file(
            &root,
            "workflows/deep-research.rhai"
        ));
        std::fs::write(root.join("workflows/deep-research.rhai"), "tampered").unwrap();
        assert!(!is_managed_bundle_file(
            &root,
            "workflows/deep-research.rhai"
        ));
    }

    #[test]
    fn extract_archive_writes_nested_skill_files() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let archive = make_test_archive(&[
            ("bundle.json", v.as_bytes()),
            ("skills/implement/SKILL.md", b"# Implement skill"),
            ("skills/implement/scripts/memory.py", b"print('memory')\n"),
            (
                "skills/implement/tests/test_memory.py",
                b"def test_memory():\n    pass\n",
            ),
        ]);

        let manifest = extract_bundle_archive(&root, &archive).unwrap();

        assert_eq!(manifest.version, "v1");
        assert_eq!(
            std::fs::read_to_string(root.join("skills/implement/SKILL.md")).unwrap(),
            "# Implement skill"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("skills/implement/scripts/memory.py")).unwrap(),
            "print('memory')\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("skills/implement/tests/test_memory.py")).unwrap(),
            "def test_memory():\n    pass\n"
        );
        assert!(manifest.checksums.contains_key("skills/implement/SKILL.md"));
        assert!(
            manifest
                .checksums
                .contains_key("skills/implement/scripts/memory.py")
        );
        assert!(
            manifest
                .checksums
                .contains_key("skills/implement/tests/test_memory.py")
        );
    }

    #[test]
    fn extract_archive_prunes_removed_nested_skill_files() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            ("skills/implement/SKILL.md", b"# Implement"),
            ("skills/implement/scripts/memory.py", b"# v1 helper\n"),
        ]);
        extract_bundle_archive(&root, &v1_archive).unwrap();
        assert!(root.join("skills/implement/scripts/memory.py").exists());

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[
            ("bundle.json", v2.as_bytes()),
            ("skills/implement/SKILL.md", b"# Implement"),
        ]);
        let manifest = extract_bundle_archive(&root, &v2_archive).unwrap();

        assert!(!root.join("skills/implement/scripts/memory.py").exists());
        assert!(
            !manifest
                .checksums
                .contains_key("skills/implement/scripts/memory.py")
        );
        assert!(manifest.checksums.contains_key("skills/implement/SKILL.md"));
    }

    #[test]
    fn extract_archive_keeps_user_modified_nested_skill_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            ("skills/implement/SKILL.md", b"# v1"),
            ("skills/implement/scripts/memory.py", b"# v1 helper\n"),
        ]);
        let manifest_v1 = extract_bundle_archive(&root, &v1_archive).unwrap();

        std::fs::write(
            root.join("skills/implement/scripts/memory.py"),
            b"# user edit\n",
        )
        .unwrap();

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[
            ("bundle.json", v2.as_bytes()),
            ("skills/implement/SKILL.md", b"# v2"),
            ("skills/implement/scripts/memory.py", b"# v2 helper\n"),
        ]);
        let manifest_v2 = extract_bundle_archive(&root, &v2_archive).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("skills/implement/scripts/memory.py")).unwrap(),
            "# user edit\n"
        );
        assert_eq!(
            manifest_v2
                .checksums
                .get("skills/implement/scripts/memory.py"),
            manifest_v1
                .checksums
                .get("skills/implement/scripts/memory.py")
        );
    }

    #[test]
    fn extract_archive_overwrites_unchanged_nested_skill_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            ("skills/implement/SKILL.md", b"# v1"),
            ("skills/implement/scripts/memory.py", b"# v1 helper\n"),
        ]);
        extract_bundle_archive(&root, &v1_archive).unwrap();

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[
            ("bundle.json", v2.as_bytes()),
            ("skills/implement/SKILL.md", b"# v2"),
            ("skills/implement/scripts/memory.py", b"# v2 helper\n"),
        ]);
        extract_bundle_archive(&root, &v2_archive).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("skills/implement/scripts/memory.py")).unwrap(),
            "# v2 helper\n"
        );
    }

    #[test]
    fn extract_archive_skips_user_modified_files() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"old\"",
            ),
        ]);
        let manifest_v1 = extract_bundle_archive(&root, &v1_archive).unwrap();

        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"user edit\"",
        )
        .unwrap();

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[
            ("bundle.json", v2.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"new\"",
            ),
        ]);
        let manifest_v2 = extract_bundle_archive(&root, &v2_archive).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"user edit\""
        );
        assert_eq!(
            manifest_v2.checksums.get("personas/researcher.toml"),
            manifest_v1.checksums.get("personas/researcher.toml")
        );
    }

    #[test]
    fn extract_archive_prunes_removed_files() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"hello\"",
            ),
        ]);
        extract_bundle_archive(&root, &v1_archive).unwrap();

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[("bundle.json", v2.as_bytes())]);
        let manifest = extract_bundle_archive(&root, &v2_archive).unwrap();

        assert!(!root.join("personas/researcher.toml").exists());
        assert!(!manifest.checksums.contains_key("personas/researcher.toml"));
    }

    #[test]
    fn extract_archive_keeps_removed_modified_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"old\"",
            ),
        ]);
        let manifest_v1 = extract_bundle_archive(&root, &v1_archive).unwrap();

        std::fs::write(
            root.join("personas/researcher.toml"),
            "instructions = \"user edit\"",
        )
        .unwrap();

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[("bundle.json", v2.as_bytes())]);
        let manifest_v2 = extract_bundle_archive(&root, &v2_archive).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"user edit\""
        );
        assert_eq!(
            manifest_v2.checksums.get("personas/researcher.toml"),
            manifest_v1.checksums.get("personas/researcher.toml")
        );
    }

    #[test]
    fn extract_archive_rejects_oversized_entry() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let big_content = vec![0u8; ARCHIVE_MAX_ENTRY_SIZE as usize + 1];
        let archive = make_test_archive(&[
            ("bundle.json", v.as_bytes()),
            ("subagents/personas/big.toml", &big_content),
        ]);

        let err = extract_bundle_archive(&root, &archive).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum size"));
    }

    #[test]
    fn extract_archive_rejects_too_many_entries() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let v = r#"{"version":"v1"}"#;
        let mut header = tar::Header::new_gnu();
        header.set_size(v.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "bundle.json", v.as_bytes())
            .unwrap();

        let small = b"x";
        for i in 0..ARCHIVE_MAX_ENTRIES {
            let path = format!("unknown/f{i}");
            let mut h = tar::Header::new_gnu();
            h.set_size(small.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            builder.append_data(&mut h, &path, &small[..]).unwrap();
        }

        let encoder = builder.into_inner().unwrap();
        let archive = encoder.finish().unwrap();

        let err = extract_bundle_archive(&root, &archive).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum entry count"));
    }

    #[test]
    fn extract_archive_skips_directory_entries() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let v = r#"{"version":"v1"}"#;
        let mut h = tar::Header::new_gnu();
        h.set_size(v.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        builder
            .append_data(&mut h, "bundle.json", v.as_bytes())
            .unwrap();

        let mut dir_h = tar::Header::new_gnu();
        dir_h.set_size(0);
        dir_h.set_mode(0o755);
        dir_h.set_entry_type(tar::EntryType::Directory);
        dir_h.set_cksum();
        builder
            .append_data(&mut dir_h, "subagents/personas/", &[] as &[u8])
            .unwrap();

        let content = b"instructions = \"hello\"";
        let mut fh = tar::Header::new_gnu();
        fh.set_size(content.len() as u64);
        fh.set_mode(0o644);
        fh.set_cksum();
        builder
            .append_data(&mut fh, "subagents/personas/researcher.toml", &content[..])
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        let archive = encoder.finish().unwrap();

        let manifest = extract_bundle_archive(&root, &archive).unwrap();

        assert_eq!(manifest.checksums.len(), 1);
        assert!(manifest.checksums.contains_key("personas/researcher.toml"));
    }

    #[test]
    fn extract_archive_missing_bundle_json_fails() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let archive = make_test_archive(&[(
            "subagents/personas/researcher.toml",
            b"instructions = \"hello\"" as &[u8],
        )]);

        let err = extract_bundle_archive(&root, &archive).unwrap_err();
        assert!(err.to_string().contains("bundle.json"));
    }

    #[test]
    fn extract_archive_handles_dot_slash_prefix() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let archive = make_test_archive(&[
            ("./bundle.json", v.as_bytes()),
            (
                "./subagents/personas/researcher.toml",
                b"instructions = \"hello\"",
            ),
            ("./skills/commit/SKILL.md", b"# Commit"),
        ]);

        let manifest = extract_bundle_archive(&root, &archive).unwrap();
        assert_eq!(manifest.version, "v1");
        assert!(manifest.checksums.contains_key("personas/researcher.toml"));
        assert!(manifest.checksums.contains_key("skills/commit/SKILL.md"));
    }

    #[test]
    fn extract_archive_overwrites_unchanged_file() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let v1 = bundle_json("v1");
        let v1_archive = make_test_archive(&[
            ("bundle.json", v1.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"old\"",
            ),
        ]);
        extract_bundle_archive(&root, &v1_archive).unwrap();

        let v2 = bundle_json("v2");
        let v2_archive = make_test_archive(&[
            ("bundle.json", v2.as_bytes()),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"new\"",
            ),
        ]);
        extract_bundle_archive(&root, &v2_archive).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("personas/researcher.toml")).unwrap(),
            "instructions = \"new\""
        );
    }

    #[test]
    fn extract_archive_skips_unknown_top_level_paths() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let archive = make_test_archive(&[
            ("bundle.json", v.as_bytes()),
            ("README.md", b"# readme"),
            ("unknown/file.txt", b"data"),
            ("subagents/personas/valid.toml", b"instructions = \"ok\""),
        ]);

        let manifest = extract_bundle_archive(&root, &archive).unwrap();
        assert_eq!(manifest.checksums.len(), 1);
        assert!(manifest.checksums.contains_key("personas/valid.toml"));
    }

    #[test]
    fn extract_archive_rejects_excessive_total_decompressed_size() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);

        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let v = r#"{"version":"v1"}"#;
        let mut h = tar::Header::new_gnu();
        h.set_size(v.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        builder
            .append_data(&mut h, "bundle.json", v.as_bytes())
            .unwrap();

        // 51 entries of 1 MB each = 51 MB > 50 MB limit.
        // Each entry is at the per-entry limit (not over), so only the aggregate check fires.
        let big = vec![0u8; ARCHIVE_MAX_ENTRY_SIZE as usize];
        for i in 0..51 {
            let path = format!("subagents/personas/p{i}.toml");
            let mut fh = tar::Header::new_gnu();
            fh.set_size(big.len() as u64);
            fh.set_mode(0o644);
            fh.set_cksum();
            builder.append_data(&mut fh, &path, big.as_slice()).unwrap();
        }

        let encoder = builder.into_inner().unwrap();
        let archive = encoder.finish().unwrap();

        let err = extract_bundle_archive(&root, &archive).unwrap_err();
        assert!(
            err.to_string()
                .contains("exceeds maximum decompressed size")
        );
    }

    // --- map_archive_path_to_cache_path tests ---

    #[test]
    fn map_archive_strips_subagents_prefix() {
        assert_eq!(
            map_archive_path_to_cache_path("subagents/personas/researcher.toml"),
            Some("personas/researcher.toml".to_string())
        );
        assert_eq!(
            map_archive_path_to_cache_path("subagents/roles/reviewer.toml"),
            Some("roles/reviewer.toml".to_string())
        );
        assert_eq!(
            map_archive_path_to_cache_path("subagents/agents/default.md"),
            Some("agents/default.md".to_string())
        );
    }

    #[test]
    fn map_archive_preserves_skills_path() {
        assert_eq!(
            map_archive_path_to_cache_path("skills/commit/SKILL.md"),
            Some("skills/commit/SKILL.md".to_string())
        );
    }

    #[test]
    fn map_archive_preserves_nested_skill_paths() {
        assert_eq!(
            map_archive_path_to_cache_path("skills/implement/scripts/memory.py"),
            Some("skills/implement/scripts/memory.py".to_string())
        );
        assert_eq!(
            map_archive_path_to_cache_path("skills/implement/tests/test_memory.py"),
            Some("skills/implement/tests/test_memory.py".to_string())
        );
    }

    #[test]
    fn sanitize_accepts_shared_data_under_skills() {
        // Non-skill directories under skills/ (e.g., shared/personas/) are
        // valid archive entries -- they carry data that skills read at runtime.
        assert_eq!(
            sanitize_relative_path("skills/shared/personas/reviewer.md"),
            Some("skills/shared/personas/reviewer.md".to_string())
        );
        assert_eq!(
            sanitize_relative_path("skills/shared/personas/implementer.md"),
            Some("skills/shared/personas/implementer.md".to_string())
        );
    }

    #[test]
    fn extract_archive_writes_shared_data_under_skills() {
        let tmp = TempDir::new().unwrap();
        let root = cache_root(&tmp);
        let v = bundle_json("v1");
        let archive = make_test_archive(&[
            ("bundle.json", v.as_bytes()),
            ("skills/review/SKILL.md", b"# Review skill"),
            ("skills/shared/personas/reviewer.md", b"You are a reviewer."),
        ]);

        let manifest = extract_bundle_archive(&root, &archive).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("skills/shared/personas/reviewer.md")).unwrap(),
            "You are a reviewer."
        );
        assert!(
            manifest
                .checksums
                .contains_key("skills/shared/personas/reviewer.md")
        );
    }

    #[test]
    fn map_archive_skips_unknown_paths() {
        assert_eq!(map_archive_path_to_cache_path("unknown/file.txt"), None);
        assert_eq!(map_archive_path_to_cache_path("README.md"), None);
        assert_eq!(map_archive_path_to_cache_path(""), None);
    }

    #[test]
    fn map_archive_rejects_traversal_under_subagents() {
        assert_eq!(
            map_archive_path_to_cache_path("subagents/personas/../../etc/passwd"),
            None
        );
    }

    // --- count_entries_by_prefix tests ---

    #[test]
    fn count_entries_by_prefix_counts_correctly() {
        let manifest = BundleManifest {
            version: "v1".to_string(),
            checksums: HashMap::from([
                ("personas/a.toml".to_string(), "abc".to_string()),
                ("personas/b.toml".to_string(), "def".to_string()),
                ("roles/r.toml".to_string(), "ghi".to_string()),
                ("skills/commit/SKILL.md".to_string(), "jkl".to_string()),
            ]),
        };
        assert_eq!(count_entries_by_prefix(&manifest, "personas/"), 2);
        assert_eq!(count_entries_by_prefix(&manifest, "roles/"), 1);
        assert_eq!(count_entries_by_prefix(&manifest, "skills/"), 1);
        assert_eq!(count_entries_by_prefix(&manifest, "agents/"), 0);
    }
}
