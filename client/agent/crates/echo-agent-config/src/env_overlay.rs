//! `ECHO_AGENT_CONFIG` / `ECHO_AGENT_CONFIG_PATH` config overlay resolution.

use std::path::{Path, PathBuf};

use crate::loader::{
    apply_version_overrides_with_registered, expand_env_vars_in_toml, normalize_config_layer,
};

/// Inline config overlay: a JSON object.
pub const ECHO_AGENT_CONFIG_ENV: &str = "ECHO_AGENT_CONFIG";

/// Path to an additional JSON or TOML config-overlay file (read by extension).
pub const ECHO_AGENT_CONFIG_PATH_ENV: &str = "ECHO_AGENT_CONFIG_PATH";

/// Hard cap on a `ECHO_AGENT_CONFIG_PATH` overlay read. Matches the untrusted-config
/// read cap in `managed_text` (`MAX_CONFIG_BYTES`, also 4 MiB): a huge file, or
/// a special node like `/dev/zero`, must never stall or OOM the agent, so the
/// read is bounded and an over-cap or non-regular file falls through to no
/// overlay, the same as the unreadable-path handling.
const MAX_OVERLAY_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
enum OverlayFormat {
    Json,
    Toml,
}

/// Which env var supplied the resolved overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlaySource {
    Inline,
    Path(PathBuf),
}

/// A resolved overlay for the merge and for `echoagent inspect`.
#[derive(Debug, Clone)]
pub struct ResolvedOverlay {
    pub source: OverlaySource,
    pub sections: Vec<String>,
    pub value: toml::Value,
}

fn env_overlay_inputs() -> (Option<String>, Option<PathBuf>) {
    if crate::paths::controlled_discovery_home().is_some() { return (None, None); }
    let inline = match std::env::var_os(ECHO_AGENT_CONFIG_ENV) {
        Some(raw) => match raw.into_string() {
            Ok(s) => Some(s),
            Err(_) => {
                tracing::warn!("ECHO_AGENT_CONFIG is not valid UTF-8; ignoring the overlay");
                None
            }
        },
        None => None,
    };
    let path = std::env::var_os(ECHO_AGENT_CONFIG_PATH_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    (inline, path)
}

pub(crate) fn load_env_overlay() -> Option<toml::Value> {
    let (inline, path) = env_overlay_inputs();
    let (overlay, source, sections) = resolve_overlay_detailed(inline.as_deref(), path.as_deref())?;
    let source_label = match source {
        OverlaySource::Inline => ECHO_AGENT_CONFIG_ENV,
        OverlaySource::Path(_) => ECHO_AGENT_CONFIG_PATH_ENV,
    };
    tracing::trace!(
        source = source_label,
        ?sections,
        "resolved ECHO_AGENT_CONFIG overlay"
    );
    Some(overlay)
}

/// The resolved overlay for `echoagent inspect`.
pub fn resolved_env_overlay() -> Option<ResolvedOverlay> {
    let (inline, path) = env_overlay_inputs();
    let (value, source, sections) = resolve_overlay_detailed(inline.as_deref(), path.as_deref())?;
    Some(ResolvedOverlay {
        source,
        sections,
        value,
    })
}

fn resolve_overlay_detailed(
    inline: Option<&str>,
    path: Option<&Path>,
) -> Option<(toml::Value, OverlaySource, Vec<String>)> {
    // Inline (`ECHO_AGENT_CONFIG`) wins only when it fully succeeds. When it is
    // absent, empty, or rejected at any stage (parse, `$VAR` expand,
    // version_overrides, normalize), fall through to the `ECHO_AGENT_CONFIG_PATH`
    // candidate so a valid file is never skipped because of a bad inline value.
    if let Some(inline) = inline
        && let Some(resolved) = resolve_inline_overlay(inline)
    {
        return Some(resolved);
    }
    resolve_path_overlay(path?)
}

#[cfg(test)]
fn resolve_overlay(inline: Option<&str>, path: Option<&Path>) -> Option<toml::Value> {
    resolve_overlay_detailed(inline, path).map(|(value, _, _)| value)
}

/// Parse and run the full pipeline for the inline candidate. Returns `None`
/// (so the caller falls through to the path) when the value is empty, fails to
/// parse, is rejected during post-processing, or reduces to an empty table.
/// Emptiness is decided by [`finalize_overlay`], after `$VAR` expand,
/// `version_overrides`, campaign stripping, and normalize, so an inline object
/// that carries only stripped keys (for example `version_overrides` or
/// `[[campaigns]]`) does not count as a usable overlay.
fn resolve_inline_overlay(inline: &str) -> Option<(toml::Value, OverlaySource, Vec<String>)> {
    let trimmed = inline.trim();
    if trimmed.is_empty() {
        return None;
    }
    let overlay = parse_overlay(trimmed, OverlayFormat::Json, ECHO_AGENT_CONFIG_ENV)?;
    finalize_overlay(overlay, ECHO_AGENT_CONFIG_ENV, OverlaySource::Inline)
}

/// Read, parse, and run the full pipeline for the `ECHO_AGENT_CONFIG_PATH` candidate.
fn resolve_path_overlay(path: &Path) -> Option<(toml::Value, OverlaySource, Vec<String>)> {
    let raw = read_capped_overlay_file(path)?;
    let format = match path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("json") => OverlayFormat::Json,
        _ => OverlayFormat::Toml,
    };
    let overlay = parse_overlay(&raw, format, ECHO_AGENT_CONFIG_PATH_ENV)?;
    finalize_overlay(
        overlay,
        ECHO_AGENT_CONFIG_PATH_ENV,
        OverlaySource::Path(path.to_path_buf()),
    )
}

/// Read `ECHO_AGENT_CONFIG_PATH` with a hard byte cap ([`MAX_OVERLAY_BYTES`]). Returns
/// `None` (fall through to no overlay, like an unreadable path) when the path is
/// missing, unreadable, not a regular file (a fifo, `/dev/zero`), or over the
/// cap. Never logs file content.
fn read_capped_overlay_file(path: &Path) -> Option<String> {
    use std::io::Read;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ECHO_AGENT_CONFIG_PATH is unreadable; ignoring the overlay");
            return None;
        }
    };
    // Reject special nodes (fifos, `/dev/zero`, ...) before reading: their
    // reported length is meaningless and they can stream without end.
    match file.metadata() {
        Ok(meta) if !meta.file_type().is_file() => {
            tracing::warn!(path = %path.display(), "ECHO_AGENT_CONFIG_PATH is not a regular file; ignoring the overlay");
            return None;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ECHO_AGENT_CONFIG_PATH is unreadable; ignoring the overlay");
            return None;
        }
    }
    // Bounded read: cap + 1 so an exactly-at-cap file still parses while an
    // over-cap file is detected and refused. `take` also guards a regular file
    // that grows between the metadata check and the read.
    let mut raw = String::new();
    if let Err(e) = file.take(MAX_OVERLAY_BYTES + 1).read_to_string(&mut raw) {
        tracing::warn!(path = %path.display(), error = %e, "ECHO_AGENT_CONFIG_PATH is unreadable; ignoring the overlay");
        return None;
    }
    if raw.len() as u64 > MAX_OVERLAY_BYTES {
        tracing::warn!(path = %path.display(), max = MAX_OVERLAY_BYTES, "ECHO_AGENT_CONFIG_PATH exceeds the max overlay size; ignoring the overlay");
        return None;
    }
    Some(raw)
}

/// Post-parse pipeline and the single overlay choke point (both
/// [`resolve_inline_overlay`] and [`resolve_path_overlay`] call it): `$VAR`
/// expand, apply this layer's `version_overrides`, take `[[campaigns]]`, confine
/// to [`crate::config_override::OVERLAY_ALLOW_PATHS`] (fail-closed: the overlay
/// carries only soft tables and leaves, everything else is dropped), and
/// normalize. The allowlist runs after `version_overrides`, so a patch cannot
/// re-inject a non-allowlisted table.
///
/// Returns `None` when the candidate is rejected (`version_overrides` fails) or
/// finalizes to an empty table (for example an object carrying no allowlisted
/// key), so it falls through to the next source instead of counting as a
/// set-but-empty layer. This is the single place emptiness is decided. The
/// `version_overrides` warning is redacted and secret-free (via
/// [`crate::version_overrides::VersionOverrideError::redacted`]).
fn finalize_overlay(
    mut overlay: toml::Value,
    source_label: &str,
    source: OverlaySource,
) -> Option<(toml::Value, OverlaySource, Vec<String>)> {
    expand_env_vars_in_toml(&mut overlay);
    if let Err(e) = apply_version_overrides_with_registered(&mut overlay) {
        tracing::warn!(source = source_label, error = %e, "config overlay `version_overrides` failed to apply; ignoring this overlay candidate");
        return None;
    }
    let _ = crate::campaigns::take_campaign_entries(&mut overlay, "env_overlay");
    if let Some(table) = overlay.as_table_mut() {
        crate::config_override::retain_overlay_allowed(table);
    }
    normalize_config_layer(&mut overlay);
    let sections: Vec<String> = overlay
        .as_table()
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    if sections.is_empty() {
        return None;
    }
    Some((overlay, source, sections))
}

fn parse_overlay(raw: &str, format: OverlayFormat, source: &str) -> Option<toml::Value> {
    let value = match format {
        OverlayFormat::Json => {
            let mut json: serde_json::Value = match serde_json::from_str(raw) {
                Ok(json) => json,
                Err(_) => {
                    tracing::warn!(
                        source,
                        "config overlay is not valid JSON; ignoring the overlay"
                    );
                    return None;
                }
            };
            strip_json_nulls(&mut json);
            match toml::Value::try_from(json) {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!(
                        source,
                        "config overlay JSON is not representable as TOML; ignoring the overlay"
                    );
                    return None;
                }
            }
        }
        OverlayFormat::Toml => match toml::from_str::<toml::Value>(raw) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    source,
                    "config overlay is not valid TOML; ignoring the overlay"
                );
                return None;
            }
        },
    };
    if !value.is_table() {
        tracing::warn!(
            source,
            "config overlay is not a table; ignoring the overlay"
        );
        return None;
    }
    Some(value)
}

fn strip_json_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            for v in map.values_mut() {
                strip_json_nulls(v);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                strip_json_nulls(item);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

#[cfg(test)]
#[path = "env_overlay_tests.rs"]
mod tests;
