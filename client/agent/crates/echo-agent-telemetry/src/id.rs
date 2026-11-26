//! Stable agent identifier.

use std::sync::{Once, OnceLock};

/// Overrides the agent ID for this process; nothing is computed or persisted.
const ENV_AGENT_ID: &str = "ECHO_AGENT_AGENT_ID";

static AGENT_ID: OnceLock<String> = OnceLock::new();
static AGENT_INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// Returns the stable agent ID: `ECHO_AGENT_AGENT_ID` if set, else the value cached
/// in `$ECHO_AGENT_HOME/agent_id`, else a machine-derived UUID computed once and
/// persisted there. The first call in a process may block while the
/// computation runs; [`prefetch_agent_id`] starts it early.
pub fn agent_id() -> String {
    AGENT_ID.get_or_init(load_or_compute_agent_id).clone()
}

/// Reads [`agent_id`] without stalling async workers on the first computation.
pub async fn agent_id_async() -> String {
    if let Some(id) = AGENT_ID.get() {
        return id.clone();
    }
    match tokio::task::spawn_blocking(agent_id).await {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(error = %err, "agent id blocking task failed; reading inline");
            agent_id()
        }
    }
}

/// Starts the agent ID computation on a background thread so later calls to
/// [`agent_id`] find the value ready, or wait only for the remaining work.
pub fn prefetch_agent_id() {
    static PREFETCH: Once = Once::new();
    PREFETCH.call_once(|| {
        if let Err(err) = std::thread::Builder::new()
            .name("agent-id-fetch".into())
            .spawn(|| {
                agent_id();
            })
        {
            tracing::warn!(error = %err, "failed to spawn the agent id prefetch thread");
        }
    });
}

/// Returns a per-process instance ID: stable across reconnects within the
/// process, new on restart.
pub fn agent_instance_id() -> String {
    AGENT_INSTANCE_ID
        .get_or_init(|| uuid::Uuid::new_v4().to_string())
        .clone()
}

fn load_or_compute_agent_id() -> String {
    if let Ok(id) = std::env::var(ENV_AGENT_ID) {
        let id = id.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }

    let cache_path = echo_agent_config::echo_agent_home().join("agent_id");
    if let Ok(cached) = std::fs::read_to_string(&cache_path) {
        let cached = cached.trim();
        if !cached.is_empty() {
            tighten_agent_id_cache_perms(&cache_path);
            return cached.to_string();
        }
    }

    let hash = compute_machine_hash();
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, hash.as_bytes()).to_string();
    let _ = write_agent_id_cache(&cache_path, &id);
    id
}

/// - macOS: mid uses unique hardware IDs (serial, UUID, SEID).
/// - Linux: /etc/machine-id is shared across containers from the same base
///   image, so include $HOSTNAME (container/host name) for uniqueness.
/// - Fallback: random UUIDv4 if mid or hostname are unavailable.
fn compute_machine_hash() -> String {
    if cfg!(target_os = "linux") {
        match std::env::var("HOSTNAME") {
            Ok(hostname) if !hostname.is_empty() => {
                let key = format!("agent_id:{hostname}");
                mid::get(&key).unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
            }
            _ => uuid::Uuid::new_v4().to_string(),
        }
    } else {
        mid::get("agent_id").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    }
}

/// Owner-only and atomic: the id is a stable device identifier, and rewriting
/// an older world-readable cache must not keep the loose mode.
fn write_agent_id_cache(path: &std::path::Path, id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    echo_agent_config::fs_atomic::write_atomically(path, id, Some(0o600))
}

/// Best effort: tightens caches written world-readable by older builds.
fn tighten_agent_id_cache_perms(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
    }

    #[test]
    fn agent_id_cache_written_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_id");
        write_agent_id_cache(&path, "test-agent-id-value").expect("write");
        assert_eq!(mode(&path), 0o600, "agent_id cache must be 0o600");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read").trim(),
            "test-agent-id-value"
        );
    }

    #[test]
    fn rewrite_over_loose_perms_cache_lands_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_id");
        std::fs::write(&path, "").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        write_agent_id_cache(&path, "fresh-id").expect("rewrite");
        assert_eq!(mode(&path), 0o600, "rewrite must not inherit loose perms");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "fresh-id");
    }

    #[test]
    fn older_world_readable_cache_is_tightened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_id");
        std::fs::write(&path, "legacy-id").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        tighten_agent_id_cache_perms(&path);
        assert_eq!(mode(&path), 0o600, "legacy cache must be tightened on read");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "legacy-id");
    }
}

/// Coarse gate for features that need a full workspace checkout; external
/// installs leave `ECHO_AGENT_ROOT` and `ECHO_AGENT_USER` unset.
pub fn has_workspace_env_markers() -> bool {
    std::env::var("ECHO_AGENT_ROOT").is_ok() && std::env::var("ECHO_AGENT_USER").is_ok()
}

/// Opt-in special-user gate for telemetry (`ECHO_AGENT_TELEMETRY_SPECIAL_USER`).
pub fn is_special_user() -> bool {
    matches!(
        std::env::var("ECHO_AGENT_TELEMETRY_SPECIAL_USER").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}
