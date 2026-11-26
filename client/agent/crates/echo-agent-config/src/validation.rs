//! Requirements layers and fail-closed enforcement.

use std::path::{Path, PathBuf};

use crate::env_bool;
use crate::loader::{apply_version_overrides_with_registered, load_toml_file};
use crate::paths::{system_config_dir, user_echo_agent_home};
use crate::version_overrides::{VersionOverrideError, apply_version_overrides};

use echo_agent_chat_proxy_types::FAIL_CLOSED_KEY;

/// `fail_closed` from a requirements table; non-bool → warn once and treat as false.
fn fail_closed_flag(requirements: &toml::Value) -> bool {
    use echo_agent_chat_proxy_types::{FailClosedFlag, fail_closed_flag_status_from_value};
    let status = fail_closed_flag_status_from_value(requirements);
    if matches!(status, FailClosedFlag::Invalid) {
        static WARN_ONCE: std::sync::Once = std::sync::Once::new();
        WARN_ONCE.call_once(|| {
            tracing::warn!(
                "requirements fail_closed is present but not a boolean \
                 (e.g. fail_closed = \"true\"); treating as false - use fail_closed = true"
            );
        });
    }
    status.is_enabled()
}

/// Env override for [`FAIL_CLOSED_KEY`]. Named for prefix-alignment
/// with `ECHO_AGENT_MANAGED_CONFIG_URL`; only applies to `requirements.toml`.
pub(crate) const FAIL_CLOSED_ENV: &str = "ECHO_AGENT_MANAGED_CONFIG_FAIL_CLOSED";

/// Where a requirements layer came from: a file on disk, or the macOS MDM
/// managed-preferences layer (admin-forced, no file). The typed split keeps a
/// caller from `exists()`/reading a layer that has no path.
#[derive(Debug, Clone)]
pub enum RequirementsSource {
    File(PathBuf),
    Mdm,
}

impl RequirementsSource {
    /// Display/provenance label — a file path string, or the synthetic MDM source
    /// id (`ai.x.echo_agent:…`). For diagnostics and matching only; the MDM layer has no
    /// file, so this is a label (`Cow<str>`), never a `Path` to open.
    pub fn label(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::File(p) => p.to_string_lossy(),
            Self::Mdm => std::borrow::Cow::Borrowed(crate::macos_managed::MDM_REQUIREMENTS_SOURCE),
        }
    }
}

/// One requirements layer: the parsed TOML and where it came from.
#[derive(Debug, Clone)]
pub struct RequirementsLayer {
    pub value: toml::Value,
    pub source: RequirementsSource,
    /// `true` = root-owned system layer. Security decisions must trust this flag,
    /// not re-derive from the source (`ECHO_AGENT_HOME`-influenced, could carry `..`).
    pub is_system: bool,
}

/// All loaded requirements layers in apply order (user first, system last).
/// Use when you need per-layer source attribution; otherwise use
/// [`load_merged_requirements`].
pub fn requirements_layers() -> Vec<RequirementsLayer> {
    let mut out = Vec::new();
    if let Some(user_path) = user_echo_agent_home().map(|g| g.join("requirements.toml"))
        && let Some(value) = load_requirements_layer(&user_path)
    {
        out.push(RequirementsLayer {
            value,
            source: RequirementsSource::File(user_path),
            is_system: false,
        });
    }
    if let Some(dir) = system_config_dir() {
        let sys_path = dir.join("requirements.toml");
        if let Some(value) = load_requirements_layer(&sys_path) {
            out.push(RequirementsLayer {
                value,
                source: RequirementsSource::File(sys_path),
                is_system: true,
            });
        }
    }
    // macOS MDM: OS-protected admin layer (forced values only). Pushed last so it
    // wins the deep-merge over the system file and cloud cache; `is_system` so
    // security decisions trust it like the root-owned layer.
    if let Some(value) = mdm_requirements_value() {
        out.push(RequirementsLayer {
            value,
            source: RequirementsSource::Mdm,
            is_system: true,
        });
    }
    out
}

/// User + system requirements deep-merged, system wins on conflict.
/// Use for read-only consumers so user pins can't bypass system policy.
pub fn load_merged_requirements() -> Option<toml::Value> {
    let mut iter = requirements_layers().into_iter();
    let mut merged = iter.next()?.value;
    for layer in iter {
        crate::loader::deep_merge_toml(&mut merged, &layer.value);
    }
    Some(merged)
}

pub(crate) fn load_requirements() -> Option<toml::Value> {
    load_user_requirements(user_echo_agent_home().as_deref())
}

/// User requirements layer from `<home>/requirements.toml`, or `None` with no
/// resolvable user home (rather than reading a cwd-relative `.echo-agent`).
fn load_user_requirements(home: Option<&Path>) -> Option<toml::Value> {
    load_requirements_layer(&home?.join("requirements.toml"))
}

pub(crate) fn load_system_requirements() -> Option<toml::Value> {
    let dir = system_config_dir()?;
    load_requirements_layer(&dir.join("requirements.toml"))
}

/// Soft-fails on errors; fail-closed enforcement lives in
/// [`validate_requirements`].
pub(crate) fn load_requirements_layer(path: &Path) -> Option<toml::Value> {
    let v = match load_toml_file(path) {
        Ok(v) if v.as_table().is_some_and(|t| !t.is_empty()) => v,
        _ => return None,
    };
    normalize_requirements_value(v, &path.display().to_string())
}

/// Strip `fail_closed` and apply `[[version_overrides]]` for a parsed
/// requirements layer (file or MDM), so every source is normalized identically.
/// `None` (skip the layer) when version_overrides are invalid for this build.
pub(crate) fn normalize_requirements_value(
    mut v: toml::Value,
    source: &str,
) -> Option<toml::Value> {
    if let Some(table) = v.as_table_mut() {
        table.remove(FAIL_CLOSED_KEY);
    }
    if let Err(e) = apply_version_overrides_with_registered(&mut v) {
        tracing::error!(
            source = %source,
            error = %e,
            "requirements rejected: invalid version_overrides; admin policy NOT applied"
        );
        return None;
    }
    Some(v)
}

/// The MDM requirements layer (read + normalized), or `None`. Shared so the
/// enforced view and the effective-config view agree.
pub(crate) fn mdm_requirements_value() -> Option<toml::Value> {
    normalize_requirements_value(
        crate::macos_managed::managed_preferences_requirements()?,
        crate::macos_managed::MDM_REQUIREMENTS_SOURCE,
    )
}

/// Errors from validating requirements layers at startup.
#[derive(Debug, thiserror::Error)]
pub enum RequirementsError {
    #[error(
        "requirements at {} has invalid version_overrides under fail_closed: {source}",
        path.display()
    )]
    InvalidVersionOverrides {
        path: PathBuf,
        #[source]
        source: VersionOverrideError,
    },
}

/// `Ok(())` unless the layer opts into fail_closed AND has invalid
/// `[[version_overrides]]` for the registered CLI version.
///
/// Re-reads the file independently from [`load_requirements_layer`]:
/// at startup both run, costing one extra small read per layer. Sharing
/// the parse would couple loader+validator APIs for negligible gain.
pub(crate) fn validate_requirements_layer(path: &Path) -> Result<(), RequirementsError> {
    let Ok(v) = load_toml_file(path) else {
        return Ok(());
    };
    validate_requirements_value(v, &RequirementsSource::File(path.to_path_buf()))
}

/// Fail-closed `[[version_overrides]]` validation for a parsed requirements layer
/// (file or MDM). Reads `fail_closed` before applying overrides so a broken patch
/// can't disable enforcement mid-load. `source` is the provenance label in the error.
fn validate_requirements_value(
    mut v: toml::Value,
    source: &RequirementsSource,
) -> Result<(), RequirementsError> {
    if v.as_table().is_none_or(|t| t.is_empty()) {
        return Ok(());
    }
    let fail_closed = resolve_fail_closed_mode(&v);
    let Ok(version) = echo_agent_version::installed_semver() else {
        return Ok(());
    };
    if let Err(e) = apply_version_overrides(&mut v, &version)
        && fail_closed
    {
        return Err(RequirementsError::InvalidVersionOverrides {
            path: PathBuf::from(source.label().as_ref()),
            source: e,
        });
    }
    Ok(())
}

/// Validates all requirements layers (user + system files, and macOS MDM). Call
/// once at startup from the binary's `main()`; exit on `Err`.
pub fn validate_requirements() -> Result<(), RequirementsError> {
    validate_user_requirements(user_echo_agent_home().as_deref())?;
    if let Some(dir) = system_config_dir() {
        validate_requirements_layer(&dir.join("requirements.toml"))?;
    }
    // MDM uses the raw value (fail_closed intact) so it's enforced like the files.
    if let Some(mdm) = crate::macos_managed::managed_preferences_requirements() {
        validate_requirements_value(mdm, &RequirementsSource::Mdm)?;
    }
    Ok(())
}

/// Validate the user requirements layer if a user home resolves; otherwise a
/// no-op (no cwd-relative `.echo-agent/requirements.toml` is read or enforced).
fn validate_user_requirements(home: Option<&Path>) -> Result<(), RequirementsError> {
    match home {
        Some(g) => validate_requirements_layer(&g.join("requirements.toml")),
        None => Ok(()),
    }
}

/// `fail_closed` for [`validate_requirements`]'s version check: the admin file flag is authoritative; the env can only TIGHTEN it (force-on), never loosen.
fn resolve_fail_closed_mode(requirements: &toml::Value) -> bool {
    fail_closed_flag(requirements) || env_bool(FAIL_CLOSED_ENV) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Even with `fail_closed = true` in the file -- enforcement is
    /// `validate_requirements`, not the loader.
    #[test]
    fn load_requirements_layer_soft_fails_on_invalid_version_overrides() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("echo-agent-vo-soft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("requirements.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
fail_closed = true
[[version_overrides]]
minimum_version = "not-a-version"
[version_overrides.features]
telemetry = true
"#
        )
        .unwrap();

        assert!(load_requirements_layer(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_requirements_layer_errs_on_fail_closed_violation() {
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("echo-agent-vo-validate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("requirements.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
fail_closed = true
[[version_overrides]]
minimum_version = "not-a-version"
"#
        )
        .unwrap();

        let err = validate_requirements_layer(&path).unwrap_err();
        assert!(matches!(
            err,
            RequirementsError::InvalidVersionOverrides { .. }
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_requirements_layer_ok_without_fail_closed() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("echo-agent-vo-soft2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("requirements.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
[[version_overrides]]
minimum_version = "not-a-version"
"#
        )
        .unwrap();

        assert!(validate_requirements_layer(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fail_closed_env_can_tighten_but_not_loosen() {
        // SAFETY: process-global env mutation, restored before return.
        let off: toml::Value = toml::from_str("fail_closed = false\n").unwrap();
        let on: toml::Value = toml::from_str("fail_closed = true\n").unwrap();
        let prior = std::env::var(FAIL_CLOSED_ENV).ok();

        // env=1 force-enables even when the file is off (tighten is allowed).
        unsafe { std::env::set_var(FAIL_CLOSED_ENV, "1") };
        assert!(resolve_fail_closed_mode(&off));
        assert!(resolve_fail_closed_mode(&on));

        // env=0 must NOT disable an admin's fail_closed=true (no local bypass).
        unsafe { std::env::set_var(FAIL_CLOSED_ENV, "0") };
        assert!(
            resolve_fail_closed_mode(&on),
            "a local env must not loosen admin fail_closed"
        );
        assert!(!resolve_fail_closed_mode(&off));

        // Unset → the admin file flag governs.
        unsafe { std::env::remove_var(FAIL_CLOSED_ENV) };
        assert!(resolve_fail_closed_mode(&on));
        assert!(!resolve_fail_closed_mode(&off));

        unsafe {
            match prior {
                Some(p) => std::env::set_var(FAIL_CLOSED_ENV, p),
                None => std::env::remove_var(FAIL_CLOSED_ENV),
            }
        }
    }

    #[test]
    fn fail_closed_flag_reads_the_opt_in() {
        let flag = |s: &str| fail_closed_flag(&toml::from_str::<toml::Value>(s).unwrap());
        assert!(flag("fail_closed = true\n"));
        assert!(!flag("fail_closed = false\n"));
        assert!(!flag("[features]\ntelemetry = true\n"));
        assert!(!flag("fail_closed = \"yes\"\n"));
        assert!(!flag(""));
    }

    #[test]
    fn fail_closed_key_is_stripped_from_returned_layer() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("echo-agent-vo-strip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("requirements.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "fail_closed = true\n[features]\ntelemetry = true\n").unwrap();

        let result = load_requirements_layer(&path).unwrap();
        assert!(
            result.get(FAIL_CLOSED_KEY).is_none(),
            "fail_closed must not leak into the returned config"
        );
        assert_eq!(result["features"]["telemetry"].as_bool(), Some(true));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_user_requirements_is_none_without_user_home() {
        // No resolvable user home => no user requirements (no cwd-relative read).
        assert!(load_user_requirements(None).is_none());
    }

    #[test]
    fn load_user_requirements_reads_layer_when_home_present() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("echo-agent-req-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("requirements.toml")).unwrap();
        writeln!(f, "[features]\ntelemetry = true\n").unwrap();

        let v = load_user_requirements(Some(&dir)).expect("layer present");
        assert_eq!(v["features"]["telemetry"].as_bool(), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_user_requirements_ok_without_user_home() {
        // No user home => nothing to validate, no error.
        assert!(validate_user_requirements(None).is_ok());
    }

    #[test]
    fn validate_user_requirements_errs_on_fail_closed_violation() {
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("echo-agent-req-validate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("requirements.toml")).unwrap();
        writeln!(
            f,
            r#"
fail_closed = true
[[version_overrides]]
minimum_version = "not-a-version"
"#
        )
        .unwrap();

        let err = validate_user_requirements(Some(&dir)).unwrap_err();
        assert!(matches!(
            err,
            RequirementsError::InvalidVersionOverrides { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The macOS MDM layer enters at the value level (no file on disk):
    /// `mdm_requirements_value` / `validate_requirements` hand the CFPreferences
    /// value straight to these, so normalize (strip `fail_closed`, keep the
    /// clamp) and enforcement (Err under `fail_closed` + a bad override) must
    /// hold with no file in the loop.
    #[test]
    fn mdm_value_normalizes_and_enforces_like_a_file() {
        let source = crate::macos_managed::MDM_REQUIREMENTS_SOURCE;

        // Effective view: fail_closed stripped, the forced clamp kept.
        let raw: toml::Value =
            toml::from_str("fail_closed = true\n[features]\nweb_fetch = false\n").unwrap();
        let normalized = normalize_requirements_value(raw, source).unwrap();
        assert!(normalized.get(FAIL_CLOSED_KEY).is_none());
        assert_eq!(normalized["features"]["web_fetch"].as_bool(), Some(false));

        // Enforcement keeps fail_closed: a bad override under fail_closed => Err,
        // the same override without fail_closed soft-fails (Ok).
        let bad: toml::Value = toml::from_str(
            "fail_closed = true\n[[version_overrides]]\nminimum_version = \"not-a-version\"\n",
        )
        .unwrap();
        assert!(matches!(
            validate_requirements_value(bad, &RequirementsSource::Mdm).unwrap_err(),
            RequirementsError::InvalidVersionOverrides { .. }
        ));
        let soft: toml::Value =
            toml::from_str("[[version_overrides]]\nminimum_version = \"not-a-version\"\n").unwrap();
        assert!(validate_requirements_value(soft, &RequirementsSource::Mdm).is_ok());
    }
}
