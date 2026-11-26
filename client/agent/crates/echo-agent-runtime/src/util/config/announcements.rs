use toml::Value as TomlValue;

/// Announcement entry received from cli-chat-proxy `/v1/settings`.
/// Re-exported from `echo-agent-announcements` for backward compatibility.
pub use echo_agent_announcements::RemoteAnnouncement;

// ---------------------------------------------------------------------------
// Announcements & tips from TOML
// ---------------------------------------------------------------------------

/// Parse `announcements` from a TOML value (inline tables or array-of-tables).
pub(crate) fn announcements_from_toml(root: &TomlValue) -> Vec<RemoteAnnouncement> {
    root.get("announcements")
        .and_then(|v| v.clone().try_into::<Vec<RemoteAnnouncement>>().ok())
        .unwrap_or_default()
}

/// Merge announcement slices in priority order. Dedup by `id`; first wins.
pub(crate) fn merge_announcements(sources: &[&[RemoteAnnouncement]]) -> Vec<RemoteAnnouncement> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut out = Vec::new();
    for source in sources {
        for a in *source {
            if let Some(ref id) = a.id
                && !seen.insert(id.clone())
            {
                continue;
            }
            out.push(a.clone());
        }
    }
    out
}

/// Dev/test override for announcements via `ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE` (a JSON
/// array of announcements). Returns `Some` only when the env var holds valid
/// JSON; an empty array (`[]`) suppresses all announcements. Every announcement
/// resolution path honors this so it works for testing regardless of source.
pub(crate) fn announcements_override() -> Option<Vec<RemoteAnnouncement>> {
    let raw = std::env::var("ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE").ok()?;
    match serde_json::from_str::<Vec<RemoteAnnouncement>>(&raw) {
        Ok(list) => Some(list),
        Err(_) => {
            tracing::warn!("invalid ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE JSON; ignoring");
            None
        }
    }
}

/// Resolve announcements from pre-loaded config layers.
///
/// Priority: requirements > remote > user config > managed config.
/// `ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE` env var overrides everything (dev-only escape hatch).
pub fn resolve_announcements(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&[RemoteAnnouncement]>,
) -> Vec<RemoteAnnouncement> {
    if let Some(list) = announcements_override() {
        return list;
    }

    let req = requirements
        .map(announcements_from_toml)
        .unwrap_or_default();
    let usr = user.map(announcements_from_toml).unwrap_or_default();
    let mgd = managed.map(announcements_from_toml).unwrap_or_default();
    let remote_slice = remote.unwrap_or_default();

    merge_announcements(&[&req, remote_slice, &usr, &mgd])
}
