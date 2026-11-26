//! macOS MDM managed-preferences layer.
//!
//! Admins push a device profile with standard-base64 (padded) TOML under
//! preference domain `ai.x.echo_agent` (`requirements_toml_base64`). Only admin-*forced*
//! values are read, so a local user can't forge it via their own preference
//! domain; trusted on every launch, independent of network/cache. `None` off macOS.

#[cfg(target_os = "macos")]
const MANAGED_PREFERENCES_DOMAIN: &str = "ai.x.echo_agent";
#[cfg(target_os = "macos")]
const REQUIREMENTS_KEY: &str = "requirements_toml_base64";

/// Synthetic source label for the MDM layer (no file on disk); diagnostics only.
pub const MDM_REQUIREMENTS_SOURCE: &str = "ai.x.echo_agent:requirements_toml_base64";

/// The MDM-forced requirements TOML, or `None` when none is forced (or not macOS).
pub(crate) fn managed_preferences_requirements() -> Option<toml::Value> {
    // Read once and cache for the process lifetime: the forced policy is fixed per
    // launch, so a mid-session profile change isn't picked up until restart — fine
    // for a short-lived CLI, and it avoids re-crossing the CoreFoundation boundary.
    static CACHED: std::sync::OnceLock<Option<toml::Value>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| managed_requirements_from(read_forced_requirements))
        .clone()
}

/// Decode the forced requirements from a raw-string reader. Split from the FFI
/// read (`read_forced_requirements`) so the forced → decode path is unit-testable
/// without CoreFoundation (the CFPreferences read/downcast itself stays FFI).
fn managed_requirements_from(read: impl FnOnce() -> Option<String>) -> Option<toml::Value> {
    decode_managed_toml(&read()?)
}

/// Decode a base64 TOML payload into a non-empty table. The forced payload is
/// used **verbatim** — `$VAR`/`${VAR}` are deliberately NOT expanded: this is
/// the trusted, non-forgeable admin layer, and expanding from the local process
/// environment would let the very user the forced check excludes influence the
/// policy (which feeds yolo / permission / minimum-version enforcement). FFI-free,
/// so unit-tested on every platform; invalid base64/UTF-8/TOML or an empty table
/// yields `None`.
fn decode_managed_toml(encoded: &str) -> Option<toml::Value> {
    use base64::Engine as _;

    // Strip all whitespace: profile tooling line-wraps payloads and the STANDARD
    // engine rejects interior whitespace.
    let compact: String = encoded
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|e| tracing::warn!("managed preference is not valid base64: {e}"))
        .ok()?;
    let toml_str = String::from_utf8(decoded)
        .map_err(|e| tracing::warn!("managed preference is not valid UTF-8: {e}"))
        .ok()?;
    let value = toml::from_str::<toml::Value>(&toml_str)
        .map_err(|e| {
            // Redact via the span-only detail: a TOML error's Display echoes the
            // offending source line, and an admin payload may carry secrets.
            tracing::warn!(
                "managed preference is not valid TOML: {}",
                crate::loader::toml_error_detail(&toml_str, &e)
            )
        })
        .ok()?;
    value
        .as_table()
        .is_some_and(|t| !t.is_empty())
        .then_some(value)
}

/// The raw forced `requirements_toml_base64` MDM string via CoreFoundation, or
/// `None`. macOS only.
#[cfg(target_os = "macos")]
fn read_forced_requirements() -> Option<String> {
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};

    // `CFPreferencesCopyAppValue` returns a +1 CFPropertyListRef (Copy rule).
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFPreferencesCopyAppValue(key: CFStringRef, application_id: CFStringRef) -> CFTypeRef;
        fn CFPreferencesAppValueIsForced(key: CFStringRef, application_id: CFStringRef) -> u8;
    }

    let cf_key = CFString::new(REQUIREMENTS_KEY);
    let cf_app = CFString::new(MANAGED_PREFERENCES_DOMAIN);

    // Trust only admin-forced values: otherwise the lookup falls through to the
    // per-user domain, which a local user can set (`defaults write ai.x.echo_agent`)
    // to forge an `is_system`-trusted layer.
    let forced = unsafe {
        CFPreferencesAppValueIsForced(cf_key.as_concrete_TypeRef(), cf_app.as_concrete_TypeRef())
    };
    if forced == 0 {
        return None;
    }

    let value_ref = unsafe {
        CFPreferencesCopyAppValue(cf_key.as_concrete_TypeRef(), cf_app.as_concrete_TypeRef())
    };
    if value_ref.is_null() {
        return None;
    }
    // Type-check before reading as text: reading a non-CFString through CFString
    // APIs is UB. `wrap_under_create_rule` owns the +1, freeing it even if the
    // downcast fails.
    let value = unsafe { CFType::wrap_under_create_rule(value_ref) };
    value.downcast_into::<CFString>().map(|s| s.to_string())
}

#[cfg(not(target_os = "macos"))]
fn read_forced_requirements() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    }

    #[test]
    fn decodes_valid_base64_toml_table() {
        let v = decode_managed_toml(&b64("allowed_sandbox_modes = [\"read-only\"]\n"))
            .expect("valid payload decodes");
        assert_eq!(
            v.get("allowed_sandbox_modes")
                .and_then(|m| m.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn decodes_line_wrapped_base64() {
        // Profile tooling line-wraps base64; interior newlines must be tolerated.
        let raw = b64("allowed_sandbox_modes = [\"read-only\"]\n");
        let wrapped = format!("{}\n{}", &raw[..4], &raw[4..]);
        assert!(decode_managed_toml(&wrapped).is_some());
    }

    /// The forced payload is used verbatim: `$VAR`/`${VAR}` must NOT be expanded,
    /// so a local user can't influence the trusted admin layer through their env.
    #[test]
    fn forced_payload_is_not_env_expanded() {
        // SAFETY: process-global env mutation, restored before return.
        let prior = std::env::var("ECHO_AGENT_MDM_NO_EXPAND_TEST").ok();
        unsafe { std::env::set_var("ECHO_AGENT_MDM_NO_EXPAND_TEST", "attacker") };
        let decoded =
            decode_managed_toml(&b64("base_url = \"${ECHO_AGENT_MDM_NO_EXPAND_TEST}/v1\"\n"));
        unsafe {
            match prior {
                Some(p) => std::env::set_var("ECHO_AGENT_MDM_NO_EXPAND_TEST", p),
                None => std::env::remove_var("ECHO_AGENT_MDM_NO_EXPAND_TEST"),
            }
        }
        assert_eq!(
            decoded
                .as_ref()
                .and_then(|v| v.get("base_url"))
                .and_then(|v| v.as_str()),
            Some("${ECHO_AGENT_MDM_NO_EXPAND_TEST}/v1"),
            "forced payload must keep ${{VAR}} literal, not expand from the user env",
        );
    }

    /// The forced gating around the FFI read: nothing forced → no layer; a forced
    /// valid payload → a layer; a forced but unparseable payload → no layer (never
    /// a partial/garbage layer). Exercises the seam without CoreFoundation.
    #[test]
    fn managed_requirements_gated_on_the_forced_read() {
        assert!(managed_requirements_from(|| None).is_none());
        assert!(
            managed_requirements_from(|| Some(b64("allowed_sandbox_modes = [\"read-only\"]\n")))
                .is_some()
        );
        assert!(managed_requirements_from(|| Some("not base64!!!".to_string())).is_none());
    }

    #[test]
    fn rejects_garbage_and_empty() {
        // not valid base64
        assert!(decode_managed_toml("not base64!!!").is_none());
        // valid base64, but the bytes aren't valid UTF-8
        let bad_utf8 = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]);
        assert!(decode_managed_toml(&bad_utf8).is_none());
        // valid base64 + valid UTF-8, but not parseable TOML
        assert!(decode_managed_toml(&b64("= not toml =")).is_none());
        // empty table → skipped (no managed preference effectively)
        assert!(decode_managed_toml(&b64("")).is_none());
    }
}
