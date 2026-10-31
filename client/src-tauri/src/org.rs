//! Enterprise organization client.
//!
//! The desktop only stores the remote HTTPS origin and a refresh credential.
//! Access tokens stay in Rust memory; React receives session metadata but never
//! bearer tokens. All authorization decisions remain on echo-agent-server.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use fs2::FileExt;
use futures::StreamExt;
use reqwest::{Method, Response, StatusCode};
#[cfg(target_os = "macos")]
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
#[cfg(target_os = "macos")]
use security_framework_sys::base::errSecItemNotFound;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;
use url::Url;
use uuid::Uuid;
#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
};

const MAX_DOCUMENT_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
const MAX_DOCUMENT_PREVIEW_BYTES: u64 = 12 * 1024 * 1024;
const MAX_DOCUMENT_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SKILL_UPLOAD_BYTES: u64 = 20 * 1024 * 1024;
const MAX_LOCAL_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JSON_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_JSON_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SSE_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SERVER_URL_CHARS: usize = 2_048;
const MAX_USERNAME_CHARS: usize = 256;
const MAX_PASSWORD_BYTES: usize = 16 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_PROFILE_FIELD_CHARS: usize = 512;
const MAX_SIGNING_FIELD_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_MESSAGE_CHARS: usize = 2_048;
const MAX_MANAGED_SKILLS: usize = 512;
const MAX_MANAGED_SKILL_FILES: usize = 512;
const MAX_MANAGED_SKILL_DEPTH: usize = 12;
const MAX_RESOURCE_ID_CHARS: usize = 512;
const MAX_QUERY_CHARS: usize = 4_096;
const MAX_TITLE_CHARS: usize = 4_096;
const MAX_TAGS: usize = 64;
const MAX_TAG_CHARS: usize = 256;
const MAX_FEEDBACK_CHARS: usize = 16_384;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MAX_FALLBACK_CREDENTIALS: usize = 32;

#[cfg(target_os = "macos")]
const CREDENTIAL_SERVICE: &str = "com.echoagent.organization.v2";
#[cfg(target_os = "macos")]
const LEGACY_CREDENTIAL_SERVICE: &str = "com.echoagent.organization";
const PROFILE_FILE: &str = "organization-profile.json";
const LOGIN_HINT_FILE: &str = "organization-login-hint.json";
const SKILL_STATE_FILE: &str = "organization-skills.json";
const SKILL_STATE_LOCK_FILE: &str = ".organization-skills.lock";
const MODEL_STATE_LOCK_FILE: &str = ".organization-model.lock";
const LOCAL_KB_SOURCES_FILE: &str = "local-knowledge-sources.json";
const ORGANIZATION_CA_PEM: &[u8] = include_bytes!("../certs/echo-agent-server-ca.pem");

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgProfile {
    server_url: String,
    username: String,
    user_id: String,
    device_id: String,
}

/// Non-secret identity fields retained independently from the authenticated
/// session. Losing or revoking a refresh token must not force users to type
/// their organization endpoint and account name again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgLoginHint {
    server_url: String,
    username: String,
}

#[derive(Debug, Clone, Default)]
struct OrgSession {
    profile: Option<OrgProfile>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    user: Option<Value>,
    bootstrap: Option<Value>,
    login_hint: Option<OrgLoginHint>,
    requires_reauthentication: bool,
}

struct OrgInner {
    client: reqwest::Client,
    session: AsyncMutex<OrgSession>,
    /// Serializes the network/download portion of managed-Skill syncs. State
    /// transactions use a separate bounded file lock so logout never waits on
    /// a potentially slow organization server.
    skill_sync: AsyncMutex<()>,
    skill_state_transaction: Mutex<()>,
    model_state_transaction: Mutex<()>,
    /// Invalidates work that started under an earlier account or preference.
    /// Package activation and the final commit both check this while holding
    /// the state transaction lock.
    skill_epoch: AtomicU64,
    /// Orders concurrent model downloads for the same account. A response
    /// from an earlier request must not overwrite a newer synchronization.
    model_epoch: AtomicU64,
    /// Changes exactly when a new organization identity is published or the
    /// current identity is cleared. Every authenticated response carries the
    /// generation it was requested under.
    account_generation: AtomicU64,
}

#[derive(Clone)]
pub struct OrgState {
    inner: Arc<OrgInner>,
}

impl Default for OrgState {
    fn default() -> Self {
        let persisted_profile = match read_optional_json::<OrgProfile>(&profile_path()) {
            Ok(profile) => profile,
            Err(error) => {
                tracing::warn!(%error, "failed to restore organization profile");
                None
            }
        };
        let profile_hint = persisted_profile
            .as_ref()
            .and_then(|profile| login_hint_from_profile(profile).ok());
        let profile = persisted_profile.and_then(|profile| match validate_profile(&profile) {
            Ok(()) => Some(profile),
            Err(error) => {
                tracing::warn!(%error, "ignored invalid persisted organization profile");
                None
            }
        });
        let stored_hint = match read_optional_json::<OrgLoginHint>(&login_hint_path()) {
            Ok(Some(hint)) => match validate_login_hint(&hint) {
                Ok(()) => Some(hint),
                Err(error) => {
                    tracing::warn!(%error, "ignored invalid organization login hint");
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "failed to restore organization login hint");
                None
            }
        };
        let login_hint = profile_hint.or(stored_hint);
        if let Some(hint) = login_hint.as_ref() {
            if let Err(error) = write_json_private(&login_hint_path(), hint) {
                tracing::warn!(%error, "failed to migrate organization login hint");
            }
        }
        let refresh_token = profile.as_ref().and_then(|profile| {
            match credential_read(&credential_account(profile)) {
                Ok(Some(token)) => match validated_token(&token, "organization refresh token") {
                    Ok(token) => Some(token),
                    Err(error) => {
                        tracing::warn!(%error, "ignored invalid persisted organization credential");
                        None
                    }
                },
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(%error, "failed to restore organization credential");
                    None
                }
            }
        });
        let requires_reauthentication = profile.is_some() && refresh_token.is_none();
        // This private CA only augments the organization HTTP client. Model
        // providers, MCP servers, and every other outbound client keep their
        // existing public-root trust policy.
        let organization_ca = reqwest::Certificate::from_pem(ORGANIZATION_CA_PEM)
            .expect("embedded organization CA certificate must be valid PEM");
        Self {
            inner: Arc::new(OrgInner {
                client: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    // Never forward organization Bearer credentials to a
                    // redirect target. Endpoints are expected to be final.
                    .redirect(reqwest::redirect::Policy::none())
                    .user_agent(format!("EchoAgent/{}", env!("CARGO_PKG_VERSION")))
                    .add_root_certificate(organization_ca)
                    .build()
                    .expect("build organization HTTP client"),
                session: AsyncMutex::new(OrgSession {
                    profile,
                    refresh_token,
                    login_hint,
                    requires_reauthentication,
                    ..Default::default()
                }),
                skill_sync: AsyncMutex::new(()),
                skill_state_transaction: Mutex::new(()),
                model_state_transaction: Mutex::new(()),
                skill_epoch: AtomicU64::new(0),
                model_epoch: AtomicU64::new(0),
                account_generation: AtomicU64::new(0),
            }),
        }
    }
}

static SHARED_ORG_STATE: OnceLock<OrgState> = OnceLock::new();

pub fn shared_state() -> OrgState {
    SHARED_ORG_STATE.get_or_init(OrgState::default).clone()
}

pub(crate) fn enforce_organization_model_lease() -> Result<bool, String> {
    let state = shared_state();
    with_model_state_transaction(&state.inner, || {
        crate::providers::enforce_organization_model_lease()
    })
}

async fn enforce_model_for_current_session(inner: &Arc<OrgInner>) -> Result<bool, String> {
    // Keep the session identity stable until the model transaction finishes.
    // Otherwise an old bootstrap failure could observe "signed out", wait for
    // a concurrent login, and then delete that new account's configuration.
    let session = inner.session.lock().await;
    with_model_state_transaction(inner, || {
        if !session_has_credential(&session) {
            crate::providers::remove_organization_model_config()
        } else {
            crate::providers::enforce_organization_model_lease()
        }
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSessionView {
    logged_in: bool,
    organization_memory_enabled: bool,
    server_url: Option<String>,
    username: Option<String>,
    requires_reauthentication: bool,
    user: Option<Value>,
    bootstrap: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgModelSyncView {
    configured: bool,
    model_id: Option<String>,
    synced_at: u64,
}

fn profile_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join(PROFILE_FILE)
}

fn login_hint_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join(LOGIN_HINT_FILE)
}

fn skill_state_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join(SKILL_STATE_FILE)
}

fn with_skill_state_transaction<T>(
    inner: &OrgInner,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_skill_state_transaction_at(inner, &skill_state_path(), operation)
}

fn with_skill_state_transaction_at<T>(
    inner: &OrgInner,
    state_path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _process_guard = inner
        .skill_state_transaction
        .lock()
        .map_err(|_| "organization Skill state lock is poisoned".to_string())?;
    let parent = state_path.parent().ok_or_else(|| {
        format!(
            "organization Skill state path has no parent: {}",
            state_path.display()
        )
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create organization state directory: {error}"))?;
    crate::paths::harden_private_dir(parent)?;
    let lock_path = parent.join(SKILL_STATE_LOCK_FILE);
    if std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "organization Skill state lock must not be a symlink: {}",
            lock_path.display()
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock_file = options
        .open(&lock_path)
        .map_err(|error| format!("open organization Skill state lock: {error}"))?;
    crate::paths::harden_private_file(&lock_path)?;
    let mut acquired = false;
    for _ in 0..200 {
        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                acquired = true;
                break;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => {
                return Err(format!(
                    "lock organization Skill state {}: {error}",
                    lock_path.display()
                ));
            }
        }
    }
    if !acquired {
        return Err("organization Skill state is busy; please try again".into());
    }
    let result = operation();
    let unlock = FileExt::unlock(&lock_file)
        .map_err(|error| format!("unlock organization Skill state: {error}"));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn invalidate_skill_sync(inner: &OrgInner) {
    inner.skill_epoch.fetch_add(1, Ordering::SeqCst);
}

fn require_current_skill_epoch(inner: &OrgInner, expected: u64) -> Result<(), String> {
    if inner.skill_epoch.load(Ordering::SeqCst) == expected {
        Ok(())
    } else {
        Err("organization account or Skill preferences changed during synchronization".into())
    }
}

fn with_model_state_transaction<T>(
    inner: &OrgInner,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _process_guard = inner
        .model_state_transaction
        .lock()
        .map_err(|_| "organization model state lock is poisoned".to_string())?;
    let parent = crate::paths::echo_agent_home_dir();
    std::fs::create_dir_all(&parent)
        .map_err(|error| format!("create organization state directory: {error}"))?;
    crate::paths::harden_private_dir(&parent)?;
    let lock_path = parent.join(MODEL_STATE_LOCK_FILE);
    if std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "organization model state lock must not be a symlink: {}",
            lock_path.display()
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock_file = options
        .open(&lock_path)
        .map_err(|error| format!("open organization model state lock: {error}"))?;
    crate::paths::harden_private_file(&lock_path)?;
    let mut acquired = false;
    for _ in 0..200 {
        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                acquired = true;
                break;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(format!("lock organization model state: {error}")),
        }
    }
    if !acquired {
        return Err("organization model state is busy; please try again".into());
    }
    let result = operation();
    let unlock = FileExt::unlock(&lock_file)
        .map_err(|error| format!("unlock organization model state: {error}"));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn invalidate_account_generation(inner: &OrgInner) {
    inner.account_generation.fetch_add(1, Ordering::SeqCst);
}

fn require_current_model_epoch(expected: u64, current: u64) -> Result<(), String> {
    if current == expected {
        Ok(())
    } else {
        Err("organization model synchronization was superseded".into())
    }
}

/// Organization packages live under the same user-global Skills tree scanned
/// by the Expert · Skills · Connectors page and by the Agent Runtime.
fn organization_skills_root() -> PathBuf {
    crate::paths::echo_agent_home_dir()
        .join("skills")
        .join("organization")
}

fn notify_skills_changed(app: &AppHandle, reason: &str) {
    let runtime = app.state::<crate::commands::AppState>();
    if let Err(error) = crate::agent_admin::request_internal_reload(&runtime, "skills") {
        if error == "agent not initialized" {
            tracing::debug!(%reason, "organization Skills changed before agent initialization");
        } else {
            tracing::warn!(%error, %reason, "failed to hot-reload organization Skills");
        }
    }
    if let Err(error) = app.emit("org://skills-changed", json!({ "reason": reason })) {
        tracing::debug!(%error, %reason, "failed to emit organization Skill change event");
    }
}

pub(crate) fn notify_models_changed(app: &AppHandle, reason: &str) {
    let app = app.clone();
    let reason = reason.to_string();
    tauri::async_runtime::spawn(async move {
        let runtime = app.state::<crate::commands::AppState>();
        // Clone the sender in a separate statement so the MutexGuard is
        // dropped before awaiting the Runtime reload future (`tokio::spawn`
        // requires the future to be Send on Windows/Tauri).
        let tx = runtime.tx.lock().unwrap().clone();
        let reload_result = if let Some(tx) = tx {
            crate::commands::reload_models_and_sync(&app, &runtime, &tx).await
        } else {
            Err("agent not initialized".to_string())
        };
        if let Err(error) = reload_result {
            if error == "agent not initialized" {
                tracing::debug!(%reason, "organization model synced before agent initialization");
            } else {
                tracing::warn!(%error, %reason, "failed to hot-reload organization model");
            }
        }
        // Consumers re-read authoritative readiness only after the Runtime
        // reload has completed (or definitively failed).
        if let Err(error) = app.emit("org://models-changed", json!({ "reason": reason })) {
            tracing::debug!(%error, %reason, "failed to emit organization model change event");
        }
    });
}

pub(crate) async fn notify_session_changed(app: &AppHandle, reason: &str) {
    let state = shared_state();
    let session = state.inner.session.lock().await;
    let view = session_view(&session);
    drop(session);
    if let Err(error) = app.emit(
        "org://session-changed",
        json!({ "reason": reason, "session": view }),
    ) {
        tracing::debug!(%error, %reason, "failed to emit organization session change event");
    }
}

pub(crate) fn local_kb_sources_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join(LOCAL_KB_SOURCES_FILE)
}

fn read_local_kb_sources_at(path: &Path, include_storage_state: bool) -> Result<Value, String> {
    if !path.exists() {
        return Ok(if include_storage_state {
            json!({ "exists": false, "sources": [] })
        } else {
            Value::Array(Vec::new())
        });
    }
    let value: Value = read_json(path)?;
    if !value.is_array() {
        return Err("local knowledge sources store must be an array".into());
    }
    Ok(if include_storage_state {
        json!({ "exists": true, "sources": value })
    } else {
        value
    })
}

pub fn read_local_kb_sources() -> Result<Value, String> {
    read_local_kb_sources_at(&local_kb_sources_path(), false)
}

#[tauri::command]
pub fn org_local_kb_sources_get(include_storage_state: Option<bool>) -> Result<Value, String> {
    read_local_kb_sources_at(
        &local_kb_sources_path(),
        include_storage_state.unwrap_or(false),
    )
}

#[tauri::command]
pub async fn org_local_kb_sources_set(
    app: AppHandle,
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
    sources: Value,
) -> Result<(), String> {
    ensure_value_size(&sources, 512 * 1024, "local knowledge sources")?;
    let items = sources
        .as_array()
        .ok_or("local knowledge sources must be an array")?;
    if items.len() > 50 {
        return Err("too many local knowledge sources".into());
    }
    let mut normalized = Vec::new();
    for item in items {
        let root = item
            .get("root")
            .and_then(Value::as_str)
            .ok_or("local knowledge source missing root")?;
        let id = item.get("id").and_then(Value::as_str).unwrap_or("local");
        let label = item.get("label").and_then(Value::as_str).unwrap_or("local");
        if root.len() > 4_096
            || id.trim().is_empty()
            || id.chars().count() > 256
            || id.chars().any(char::is_control)
            || label.chars().count() > MAX_PROFILE_FIELD_CHARS
            || label.chars().any(char::is_control)
        {
            return Err("local knowledge source fields are invalid or too long".into());
        }
        // Persist only a root already granted by native selection or restored
        // from trusted native state. Otherwise a forged descriptor could turn
        // into a filesystem grant after the next application restart.
        let path = filesystem.require_workspace(root)?;
        normalized.push(json!({
            "id": id,
            "kind": "local-folder",
            "label": label,
            "root": path.to_string_lossy(),
            "resourceId": crate::resource_identity::identity(&path)?.id,
            "enabled": item.get("enabled").and_then(Value::as_bool).unwrap_or(true)
        }));
    }
    write_json_private(&local_kb_sources_path(), &normalized)?;
    // Personal knowledge availability is session-scoped. Reconcile every live
    // session so tasks that selected it see folder changes without restarting;
    // tasks that did not select it remain detached.
    crate::org_mcp::reconcile_all_sessions(&app).await;
    crate::personal_knowledge::schedule_rebuild(app, false);
    Ok(())
}

pub(crate) async fn local_knowledge_allowed() -> bool {
    let state = shared_state();
    let has_credential = {
        let session = state.inner.session.lock().await;
        session_has_credential(&session)
    };
    if !has_credential {
        return true;
    }
    match update_bootstrap(&state.inner).await {
        Ok(bootstrap) => bootstrap
            .get("policy")
            .and_then(|policy| policy.get("allowLocalKnowledge"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[tauri::command]
pub async fn personal_knowledge_allowed() -> bool {
    local_knowledge_allowed().await
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn fallback_credentials_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("organization-credentials.json")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = read_file_bounded(path, MAX_LOCAL_JSON_BYTES, "JSON store")?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
        Ok(_) => read_json(path).map(Some),
    }
}

fn read_json_or_default_if_missing<T>(path: &Path) -> Result<T, String>
where
    T: for<'de> Deserialize<'de> + Default,
{
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
        Ok(_) => read_json(path),
    }
}

fn read_file_bounded(path: &Path, max_bytes: u64, label: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "{label} must be a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "{label} exceeds the safety limit: {}",
            path.display()
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("inspect opened {}: {error}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() > max_bytes {
        return Err(format!(
            "{label} exceeds the safety limit: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{label} exceeds the safety limit: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

fn write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    if bytes.len() as u64 > MAX_LOCAL_JSON_BYTES {
        return Err("JSON store exceeds 16 MiB".into());
    }
    crate::paths::write_private_file(path, &bytes)
}

fn credential_account(profile: &OrgProfile) -> String {
    let mut digest = Sha256::new();
    digest.update(profile.server_url.as_bytes());
    digest.update(b"\0");
    digest.update(profile.user_id.as_bytes());
    format!("echoagent-org-{:x}", digest.finalize())
}

fn signing_key_account(profile: &OrgProfile) -> String {
    format!("{}-signing-key", credential_account(profile))
}

#[cfg(target_os = "macos")]
fn keychain_write(service: &str, account: &str, secret: &str) -> Result<(), String> {
    if secret.len() > MAX_TOKEN_BYTES {
        return Err("organization credential exceeds the safety limit".into());
    }
    set_generic_password(service, account, secret.as_bytes())
        .map_err(|error| format!("write macOS Keychain credential: {error}"))
}

#[cfg(target_os = "macos")]
fn keychain_read(service: &str, account: &str) -> Result<Option<Vec<u8>>, String> {
    let bytes = match get_generic_password(service, account) {
        Ok(bytes) => bytes,
        Err(error) if error.code() == errSecItemNotFound => return Ok(None),
        Err(error) => return Err(format!("read macOS Keychain credential: {error}")),
    };
    Ok(Some(bytes))
}

#[cfg(target_os = "macos")]
fn legacy_keychain_cli_read(account: &str) -> Result<Option<Vec<u8>>, String> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-a",
            account,
            "-s",
            LEGACY_CREDENTIAL_SERVICE,
            "-w",
        ])
        .output()
        .map_err(|error| format!("open macOS Keychain compatibility reader: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let mut bytes = output.stdout;
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    Ok(Some(bytes))
}

#[cfg(target_os = "macos")]
fn legacy_keychain_cli_delete(account: &str) -> Result<(), String> {
    let status = Command::new("/usr/bin/security")
        .args([
            "delete-generic-password",
            "-a",
            account,
            "-s",
            LEGACY_CREDENTIAL_SERVICE,
        ])
        .status()
        .map_err(|error| format!("open macOS Keychain compatibility cleanup: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("macOS Keychain compatibility cleanup was rejected".into())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn credential_write(account: &str, secret: &str) -> Result<(), String> {
    keychain_write(CREDENTIAL_SERVICE, account, secret)
}

#[cfg(target_os = "macos")]
pub(crate) fn credential_read(account: &str) -> Result<Option<String>, String> {
    let (bytes, migrate) = match keychain_read(CREDENTIAL_SERVICE, account)? {
        Some(bytes) => (Some(bytes), false),
        None => match keychain_read(LEGACY_CREDENTIAL_SERVICE, account) {
            Ok(value) => (value, true),
            // Releases before 0.3.10 created Keychain entries through the
            // `security` tool. Their ACL can reject direct process access
            // after an application update, while the original tool remains
            // authorized. Read once through that compatibility path and copy
            // into the stable v2 service owned by EchoAgent.
            Err(direct_error) => match legacy_keychain_cli_read(account)? {
                Some(bytes) => (Some(bytes), true),
                None => return Err(direct_error),
            },
        },
    };
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err("organization credential exceeds the safety limit".into());
    }
    let secret = String::from_utf8(bytes)
        .map_err(|_| "macOS Keychain credential is not valid UTF-8".to_string())?;
    // Idempotent migration. Keeping the legacy item until explicit logout
    // avoids a destructive cut-over if the process exits between operations.
    if migrate {
        keychain_write(CREDENTIAL_SERVICE, account, &secret)?;
    }
    Ok(Some(secret))
}

#[cfg(target_os = "macos")]
pub(crate) fn credential_delete(account: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    match delete_generic_password(CREDENTIAL_SERVICE, account) {
        Ok(()) => {}
        Err(error) if error.code() == errSecItemNotFound => {}
        Err(error) => errors.push(format!("delete macOS Keychain credential: {error}")),
    }
    match delete_generic_password(LEGACY_CREDENTIAL_SERVICE, account) {
        Ok(()) => {}
        Err(error) if error.code() == errSecItemNotFound => {}
        Err(direct_error) => {
            if let Err(compatibility_error) = legacy_keychain_cli_delete(account) {
                errors.push(format!(
                    "delete legacy macOS Keychain credential: {direct_error}; {compatibility_error}"
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

// Windows uses DPAPI directly: ciphertext is bound to the current OS user and
// machine. Keep the raw DPAPI blob on disk so files created by the retired
// PowerShell implementation remain readable.
#[cfg(target_os = "windows")]
#[derive(Default)]
struct DpapiBuffer(CRYPT_INTEGER_BLOB);

#[cfg(target_os = "windows")]
impl DpapiBuffer {
    fn to_vec(&self) -> Result<Vec<u8>, String> {
        if self.0.cbData == 0 {
            return Ok(Vec::new());
        }
        if self.0.pbData.is_null() {
            return Err("DPAPI returned a null output buffer".into());
        }
        // SAFETY: CryptProtectData/CryptUnprotectData returned this allocation
        // and cbData is its initialized byte length. Copy before the allocation
        // is zeroed and released by Drop.
        Ok(unsafe { std::slice::from_raw_parts(self.0.pbData, self.0.cbData as usize) }.to_vec())
    }
}

#[cfg(target_os = "windows")]
impl Drop for DpapiBuffer {
    fn drop(&mut self) {
        if self.0.pbData.is_null() {
            return;
        }
        // SAFETY: DPAPI allocates output with LocalAlloc. Zeroing before
        // LocalFree prevents decrypted credentials lingering in freed memory.
        unsafe {
            std::ptr::write_bytes(self.0.pbData, 0, self.0.cbData as usize);
            let result = LocalFree(self.0.pbData.cast());
            debug_assert!(result.is_null(), "LocalFree rejected a DPAPI buffer");
        }
        self.0 = CRYPT_INTEGER_BLOB::default();
    }
}

#[cfg(target_os = "windows")]
fn dpapi_input(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, String> {
    let length = u32::try_from(bytes.len()).map_err(|_| "DPAPI input exceeds 4 GiB")?;
    Ok(CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: if bytes.is_empty() {
            std::ptr::null_mut()
        } else {
            bytes.as_ptr().cast_mut()
        },
    })
}

#[cfg(target_os = "windows")]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let input = dpapi_input(plaintext)?;
    let mut output = DpapiBuffer::default();
    // SAFETY: all optional pointers are null as allowed by CryptProtectData;
    // input points to plaintext for input.cbData bytes and output is writable.
    let success = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.0,
        )
    };
    if success == 0 {
        return Err(format!(
            "DPAPI encryption failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    output.to_vec()
}

#[cfg(target_os = "windows")]
fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let input = dpapi_input(ciphertext)?;
    let mut output = DpapiBuffer::default();
    // SAFETY: all optional pointers are null as allowed by CryptUnprotectData;
    // input points to ciphertext for input.cbData bytes and output is writable.
    let success = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.0,
        )
    };
    if success == 0 {
        return Err(format!(
            "DPAPI decryption failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    output.to_vec()
}

#[cfg(target_os = "windows")]
pub(crate) fn credential_write(account: &str, secret: &str) -> Result<(), String> {
    if secret.len() > MAX_TOKEN_BYTES {
        return Err("organization credential exceeds the safety limit".into());
    }
    let path = crate::paths::echo_agent_home_dir().join(format!(".{account}.dpapi"));
    let encrypted = dpapi_protect(secret.as_bytes())?;
    crate::paths::write_private_file(&path, &encrypted)
        .map_err(|e| format!("write DPAPI credential {}: {e}", path.display()))
}

#[cfg(target_os = "windows")]
pub(crate) fn credential_read(account: &str) -> Result<Option<String>, String> {
    let path = crate::paths::echo_agent_home_dir().join(format!(".{account}.dpapi"));
    if !path.exists() {
        return Ok(None);
    }
    let encrypted = read_file_bounded(&path, MAX_TOKEN_BYTES as u64 * 4, "DPAPI credential file")?;
    let plaintext = dpapi_unprotect(&encrypted)?;
    if plaintext.len() > MAX_TOKEN_BYTES {
        return Err("organization credential exceeds the safety limit".into());
    }
    String::from_utf8(plaintext)
        .map(Some)
        .map_err(|e| format!("DPAPI credential is not valid UTF-8: {e}"))
}

#[cfg(target_os = "windows")]
pub(crate) fn credential_delete(account: &str) -> Result<(), String> {
    let path = crate::paths::echo_agent_home_dir().join(format!(".{account}.dpapi"));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("delete DPAPI credential: {e}")),
    }
}

// Linux builds without a Secret Service session fall back to an owner-only file.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn fallback_credential_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_fallback_credentials() -> Result<HashMap<String, String>, String> {
    let values =
        read_json_or_default_if_missing::<HashMap<String, String>>(&fallback_credentials_path())?;
    if values.len() > MAX_FALLBACK_CREDENTIALS
        || values.iter().any(|(account, secret)| {
            account.is_empty()
                || account.chars().count() > 256
                || account.chars().any(char::is_control)
                || secret.len() > MAX_TOKEN_BYTES
        })
    {
        return Err("organization credential store is invalid or oversized".into());
    }
    Ok(values)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn credential_write(account: &str, secret: &str) -> Result<(), String> {
    if secret.len() > MAX_TOKEN_BYTES {
        return Err("organization credential exceeds the safety limit".into());
    }
    let _guard = fallback_credential_lock()
        .lock()
        .map_err(|_| "organization credential lock is poisoned".to_string())?;
    let mut values = read_fallback_credentials()?;
    if !values.contains_key(account) && values.len() >= MAX_FALLBACK_CREDENTIALS {
        return Err("organization credential store contains too many accounts".into());
    }
    values.insert(account.to_string(), secret.to_string());
    write_json_private(&fallback_credentials_path(), &values)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn credential_read(account: &str) -> Result<Option<String>, String> {
    let _guard = fallback_credential_lock()
        .lock()
        .map_err(|_| "organization credential lock is poisoned".to_string())?;
    Ok(read_fallback_credentials()?.remove(account))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn credential_delete(account: &str) -> Result<(), String> {
    let _guard = fallback_credential_lock()
        .lock()
        .map_err(|_| "organization credential lock is poisoned".to_string())?;
    let mut values = read_fallback_credentials()?;
    values.remove(account);
    write_json_private(&fallback_credentials_path(), &values)
}

fn normalize_server_url(raw: &str) -> Result<String, String> {
    if raw.chars().count() > MAX_SERVER_URL_CHARS {
        return Err("organization server URL is too long".into());
    }
    let mut url = Url::parse(raw.trim()).map_err(|e| format!("invalid server URL: {e}"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(
            "organization server must use HTTPS (HTTP is only allowed for localhost)".into(),
        );
    }
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("server URL must not contain credentials, query, or fragment".into());
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn validate_login_hint(hint: &OrgLoginHint) -> Result<(), String> {
    if normalize_server_url(&hint.server_url)? != hint.server_url {
        return Err("organization login hint contains a non-canonical server URL".into());
    }
    if hint.username.trim().is_empty()
        || hint.username.chars().count() > MAX_USERNAME_CHARS
        || hint.username.chars().any(char::is_control)
    {
        return Err("organization login hint username is invalid or too long".into());
    }
    Ok(())
}

fn login_hint_from_profile(profile: &OrgProfile) -> Result<OrgLoginHint, String> {
    let hint = OrgLoginHint {
        server_url: profile.server_url.clone(),
        username: profile.username.clone(),
    };
    validate_login_hint(&hint)?;
    Ok(hint)
}

fn validate_profile(profile: &OrgProfile) -> Result<(), String> {
    let _ = login_hint_from_profile(profile)?;
    for (label, value) in [
        ("user id", profile.user_id.as_str()),
        ("device id", profile.device_id.as_str()),
    ] {
        if value.trim().is_empty()
            || value.chars().count() > MAX_PROFILE_FIELD_CHARS
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "organization profile {label} is invalid or too long"
            ));
        }
    }
    Ok(())
}

fn session_has_credential(session: &OrgSession) -> bool {
    session.profile.is_some() && session.refresh_token.is_some()
}

fn validated_token(value: &str, label: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > MAX_TOKEN_BYTES || value.chars().any(char::is_control) {
        return Err(format!("{label} is invalid or exceeds the safety limit"));
    }
    Ok(value.to_string())
}

fn bounded_message(value: &str) -> String {
    value.chars().take(MAX_ERROR_MESSAGE_CHARS).collect()
}

fn ensure_value_size(value: &Value, max_bytes: usize, label: &str) -> Result<(), String> {
    let size = serde_json::to_vec(value)
        .map_err(|error| format!("serialize {label}: {error}"))?
        .len();
    if size > max_bytes {
        return Err(format!("{label} exceeds the safety limit"));
    }
    Ok(())
}

fn validate_bounded_text(
    value: &str,
    label: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<(), String> {
    if (!allow_empty && value.trim().is_empty())
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(format!("{label} is invalid or too long"));
    }
    Ok(())
}

fn validate_resource_id(value: &str, label: &str) -> Result<(), String> {
    validate_bounded_text(value, label, MAX_RESOURCE_ID_CHARS, false)
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

fn clear_local_session(
    inner: &OrgInner,
    session: &mut OrgSession,
    profile: Option<&OrgProfile>,
    requires_reauthentication: bool,
) -> Result<(), String> {
    crate::org_mcp::set_capability_enabled(false);
    let mut errors = Vec::new();
    let login_hint = profile
        .and_then(|profile| match login_hint_from_profile(profile) {
            Ok(hint) => Some(hint),
            Err(error) => {
                errors.push(error);
                None
            }
        })
        .or_else(|| session.login_hint.clone());
    if let Some(hint) = login_hint.as_ref() {
        if let Err(error) = write_json_private(&login_hint_path(), hint) {
            errors.push(format!("persist organization login hint: {error}"));
        }
    }
    invalidate_account_generation(inner);
    // Revoke Runtime paths while the account's signing credential is still
    // available for strict sidecar validation. Every cleanup step remains
    // best-effort so a credential-store failure cannot preserve local access.
    if let Err(error) = deactivate_managed_skills(inner) {
        errors.push(error);
    }
    if let Some(profile) = profile {
        if let Err(error) = credential_delete(&credential_account(profile)) {
            errors.push(error);
        }
        if let Err(error) = credential_delete(&signing_key_account(profile)) {
            errors.push(error);
        }
    }
    if let Err(error) = with_model_state_transaction(inner, || {
        let mut transaction_errors = Vec::new();
        if let Err(error) = crate::providers::remove_organization_model_config() {
            transaction_errors.push(format!("remove organization model config: {error}"));
        }
        if let Err(error) = std::fs::remove_file(profile_path()) {
            if error.kind() != std::io::ErrorKind::NotFound {
                transaction_errors.push(format!("delete organization profile: {error}"));
            }
        }
        if transaction_errors.is_empty() {
            Ok(())
        } else {
            Err(transaction_errors.join("; "))
        }
    }) {
        errors.push(error);
    }
    *session = OrgSession {
        login_hint,
        requires_reauthentication,
        ..Default::default()
    };
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn read_response_bounded(
    response: Response,
    max_bytes: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!("{label} exceeds the response safety limit"));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("read {label}: {error}"))?;
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| format!("{label} length overflow"))?;
        if next_len as u64 > max_bytes {
            return Err(format!("{label} exceeds the response safety limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn response_data(response: Response) -> Result<Value, String> {
    let status = response.status();
    let bytes =
        read_response_bounded(response, MAX_JSON_RESPONSE_BYTES, "organization response").await?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("decode server response: {e}"))?;
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or(status.as_u16() as i64);
    if !status.is_success() || code != 0 {
        let message = bounded_message(
            value
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("organization request failed"),
        );
        if status.is_server_error() {
            return Err(format!(
                "organization server unavailable (HTTP {}): {message}",
                status.as_u16()
            ));
        }
        return Err(message);
    }
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
}

async fn refresh(
    inner: &Arc<OrgInner>,
    rejected_access: &str,
    expected: &AccountContext,
) -> Result<(), String> {
    let (profile, refresh_token, prior_access) = {
        let session = inner.session.lock().await;
        require_account_context_locked(inner, &session, expected)?;
        if session
            .access_token
            .as_deref()
            .is_some_and(|token| token != rejected_access)
        {
            return Ok(());
        }
        let profile = session
            .profile
            .clone()
            .ok_or("not signed in to an organization")?;
        let refresh_token = session
            .refresh_token
            .clone()
            .ok_or("organization session expired")?;
        (profile, refresh_token, session.access_token.clone())
    };
    validated_token(&refresh_token, "organization refresh token")?;
    let response = inner
        .client
        .post(endpoint(&profile.server_url, "/api/v1/auth/refresh"))
        .json(&json!({ "refreshToken": refresh_token }))
        .send()
        .await
        .map_err(|e| format!("refresh organization session: {e}"))?;
    let status = response.status();
    let data = match response_data(response).await {
        Ok(data) => data,
        Err(error) => {
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                let mut session = inner.session.lock().await;
                if require_account_context_locked(inner, &session, expected).is_err() {
                    return Err("organization account changed while refresh was in flight".into());
                }
                let cleanup = clear_local_session(inner, &mut session, Some(&profile), true);
                return Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup_error) => {
                        format!("{error}; local session cleanup: {cleanup_error}")
                    }
                });
            }
            return Err(error);
        }
    };
    let access = data
        .get("accessToken")
        .and_then(Value::as_str)
        .ok_or("refresh response missing access token")?;
    let next_refresh = data
        .get("refreshToken")
        .and_then(Value::as_str)
        .ok_or("refresh response missing refresh token")?;
    let access = validated_token(access, "organization access token")?;
    let next_refresh = validated_token(next_refresh, "organization refresh token")?;
    let mut session = inner.session.lock().await;
    require_account_context_locked(inner, &session, expected)?;
    if session.access_token != prior_access {
        // Another refresh already won. Never let a later response roll its
        // tokens back, even though both requests used the same account.
        return Ok(());
    }
    credential_write(&credential_account(&profile), &next_refresh)?;
    session.access_token = Some(access);
    session.refresh_token = Some(next_refresh);
    session.requires_reauthentication = false;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccountContext {
    account_id: String,
    generation: u64,
}

struct AuthenticatedHttpResponse {
    response: Response,
    context: AccountContext,
}

fn account_context_matches(
    expected: &AccountContext,
    generation: u64,
    current_account: Option<&str>,
) -> bool {
    expected.generation == generation && current_account == Some(expected.account_id.as_str())
}

fn session_account_id(session: &OrgSession) -> Option<String> {
    session.profile.as_ref().map(credential_account)
}

fn require_account_context_locked(
    inner: &OrgInner,
    session: &OrgSession,
    expected: &AccountContext,
) -> Result<(), String> {
    let account = session_account_id(session);
    if account_context_matches(
        expected,
        inner.account_generation.load(Ordering::SeqCst),
        account.as_deref(),
    ) {
        Ok(())
    } else {
        Err("organization account changed while request was in flight".into())
    }
}

fn require_account_context_generation(
    inner: &OrgInner,
    expected: &AccountContext,
) -> Result<(), String> {
    if inner.account_generation.load(Ordering::SeqCst) == expected.generation {
        Ok(())
    } else {
        Err("organization account changed while request was in flight".into())
    }
}

async fn require_account_context(
    inner: &OrgInner,
    expected: &AccountContext,
) -> Result<(), String> {
    let session = inner.session.lock().await;
    require_account_context_locked(inner, &session, expected)
}

async fn auth_snapshot(inner: &Arc<OrgInner>) -> Result<(String, String, AccountContext), String> {
    let session = inner.session.lock().await;
    let profile = session
        .profile
        .as_ref()
        .ok_or("not signed in to an organization")?;
    let access = session.access_token.clone().unwrap_or_default();
    Ok((
        profile.server_url.clone(),
        access,
        AccountContext {
            account_id: credential_account(profile),
            generation: inner.account_generation.load(Ordering::SeqCst),
        },
    ))
}

async fn authenticated_response(
    inner: &Arc<OrgInner>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<AuthenticatedHttpResponse, String> {
    authenticated_response_for_context(inner, method, path, body, None).await
}

async fn authenticated_response_for_context(
    inner: &Arc<OrgInner>,
    method: Method,
    path: &str,
    body: Option<Value>,
    expected_context: Option<&AccountContext>,
) -> Result<AuthenticatedHttpResponse, String> {
    if path.len() > 8_192 || !path.starts_with('/') {
        return Err("organization request path is invalid or too long".into());
    }
    if body.as_ref().is_some_and(|value| {
        serde_json::to_vec(value)
            .map(|bytes| bytes.len() > MAX_JSON_REQUEST_BYTES)
            .unwrap_or(true)
    }) {
        return Err("organization request body exceeds 4 MiB".into());
    }
    let mut original_context: Option<AccountContext> = None;
    for attempt in 0..2 {
        let (base, access, context) = auth_snapshot(inner).await?;
        if expected_context.is_some_and(|expected| expected != &context) {
            return Err("organization account changed before request was sent".into());
        }
        if original_context
            .as_ref()
            .is_some_and(|original| original != &context)
        {
            return Err("organization account changed before request retry".into());
        }
        original_context.get_or_insert_with(|| context.clone());
        if access.is_empty() {
            refresh(inner, "", &context).await?;
            continue;
        }
        let mut request = inner
            .client
            .request(method.clone(), endpoint(&base, path))
            .bearer_auth(&access);
        if let Some(value) = body.clone() {
            request = request.json(&value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("organization server unreachable: {e}"))?;
        if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
            refresh(inner, &access, &context).await?;
            continue;
        }
        require_account_context(inner, &context).await?;
        return Ok(AuthenticatedHttpResponse { response, context });
    }
    Err("organization session expired".into())
}

async fn authenticated_json(
    inner: &Arc<OrgInner>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    Ok(authenticated_json_with_context(inner, method, path, body)
        .await?
        .0)
}

async fn authenticated_json_with_context(
    inner: &Arc<OrgInner>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<(Value, AccountContext), String> {
    authenticated_json_with_expected_context(inner, method, path, body, None).await
}

async fn authenticated_json_for_context(
    inner: &Arc<OrgInner>,
    method: Method,
    path: &str,
    body: Option<Value>,
    expected_context: &AccountContext,
) -> Result<Value, String> {
    Ok(
        authenticated_json_with_expected_context(inner, method, path, body, Some(expected_context))
            .await?
            .0,
    )
}

async fn authenticated_json_with_expected_context(
    inner: &Arc<OrgInner>,
    method: Method,
    path: &str,
    body: Option<Value>,
    expected_context: Option<&AccountContext>,
) -> Result<(Value, AccountContext), String> {
    let authenticated =
        authenticated_response_for_context(inner, method, path, body, expected_context).await?;
    let data = response_data(authenticated.response).await?;
    require_account_context(inner, &authenticated.context).await?;
    Ok((data, authenticated.context))
}

/// Download the organization's complete chat credential and persist it using
/// the same provider/model schema as models created manually in Settings.
async fn sync_organization_model_config(inner: &Arc<OrgInner>) -> Result<Option<String>, String> {
    sync_organization_model_config_for_context(inner, None).await
}

async fn sync_organization_model_config_for_context(
    inner: &Arc<OrgInner>,
    expected_context: Option<&AccountContext>,
) -> Result<Option<String>, String> {
    if let Some(expected_context) = expected_context {
        require_account_context(inner, expected_context).await?;
    }
    let expected_model_epoch = inner
        .model_epoch
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);
    let (data, request_context) = authenticated_json_with_expected_context(
        inner,
        Method::GET,
        "/api/v1/client/model-config",
        None,
        expected_context,
    )
    .await?;
    let credential_error = data
        .get("credentialError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let configured = data
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let required = |key: &str, max_bytes: usize| -> Result<String, String> {
        let value = data
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("organization model config missing {key}"))?;
        if value.len() > max_bytes || value.chars().any(char::is_control) {
            return Err(format!(
                "organization model config {key} is invalid or too long"
            ));
        }
        Ok(value.to_string())
    };
    let downloaded: Result<Option<crate::providers::OrganizationModelConfig>, String> =
        if credential_error || !configured {
            Ok(None)
        } else {
            let lease_until = {
                let session = inner.session.lock().await;
                require_account_context_locked(inner, &session, &request_context).and_then(|_| {
                    session
                        .bootstrap
                        .as_ref()
                        .and_then(|bootstrap| bootstrap.get("policy"))
                        .and_then(|policy| policy.get("expiresAt"))
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "organization policy missing model lease".to_string())
                })
            };
            lease_until.and_then(|lease_until| {
                Ok(Some(crate::providers::OrganizationModelConfig {
                    provider: required("chatProvider", MAX_PROFILE_FIELD_CHARS)?,
                    model: required("chatModel", MAX_PROFILE_FIELD_CHARS)?,
                    base_url: required("chatBaseUrl", MAX_SERVER_URL_CHARS)?,
                    api_key: required("chatKey", MAX_TOKEN_BYTES)?,
                    lease_until,
                }))
            })
        };
    with_model_state_transaction(inner, || {
        require_current_model_epoch(
            expected_model_epoch,
            inner.model_epoch.load(Ordering::SeqCst),
        )?;
        require_account_context_generation(inner, &request_context)?;
        let persisted_profile = read_json::<OrgProfile>(&profile_path())?;
        validate_profile(&persisted_profile)?;
        if credential_account(&persisted_profile) != request_context.account_id {
            return Err("organization account changed before model configuration commit".into());
        }
        match downloaded {
            Ok(Some(downloaded)) => {
                crate::providers::save_organization_model_config(downloaded).map(Some)
            }
            Ok(None) => {
                crate::providers::remove_organization_model_config()?;
                if credential_error {
                    Err("organization model credential cannot be decrypted".into())
                } else {
                    Ok(None)
                }
            }
            Err(validation_error) => match crate::providers::remove_organization_model_config() {
                Ok(_) => Err(validation_error),
                Err(cleanup_error) => Err(format!(
                    "{validation_error}; failed to clear invalid organization model configuration: {cleanup_error}"
                )),
            },
        }
    })
}

pub(crate) async fn mcp_json(
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let result = authenticated_json(&shared_state().inner, method, path, body).await;
    if result
        .as_ref()
        .is_err_and(|error| is_organization_capability_failure(error))
    {
        crate::org_mcp::set_capability_enabled(false);
    }
    result
}

pub(crate) async fn mcp_ask(input: Value) -> Result<Value, String> {
    let result = mcp_ask_inner(input).await;
    if result
        .as_ref()
        .is_err_and(|error| is_organization_capability_failure(error))
    {
        crate::org_mcp::set_capability_enabled(false);
    }
    result
}

async fn mcp_ask_inner(input: Value) -> Result<Value, String> {
    let state = shared_state();
    let authenticated = authenticated_response(
        &state.inner,
        Method::POST,
        "/api/v1/knowledge/ask",
        Some(input),
    )
    .await?;
    let response = authenticated.response;
    if !response.status().is_success() {
        return Err(format!("knowledge ask: HTTP {}", response.status()));
    }
    let bytes = read_response_bounded(response, MAX_SSE_TOTAL_BYTES, "knowledge answer").await?;
    require_account_context(&state.inner, &authenticated.context).await?;
    let mut pending = bytes;
    let mut events = drain_sse_events(&mut pending)?;
    if !pending.iter().all(u8::is_ascii_whitespace) {
        events.push(parse_sse_block(&pending)?);
    }
    for (event, data) in events {
        if event == "final" {
            return Ok(data);
        }
        if event == "error" {
            let message = data
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("organization knowledge service returned an error");
            return Err(message.to_string());
        }
    }
    Err("knowledge answer stream ended without a final event".into())
}

async fn update_bootstrap(inner: &Arc<OrgInner>) -> Result<Value, String> {
    Ok(update_bootstrap_with_context(inner, None).await?.0)
}

async fn update_bootstrap_with_context(
    inner: &Arc<OrgInner>,
    expected_context: Option<&AccountContext>,
) -> Result<(Value, AccountContext), String> {
    let capability_context = match expected_context {
        Some(context) => Some(context.clone()),
        None => active_account_context(inner).await.ok(),
    };
    let result = update_bootstrap_with_context_inner(inner, expected_context).await;
    match result.as_ref() {
        Ok((bootstrap, _)) => {
            crate::org_mcp::set_capability_enabled(bootstrap_enables_organization_memory(
                bootstrap,
            ));
        }
        Err(_) => {
            // A late failure from an account that has already been replaced
            // must not detach tools belonging to the new identity.
            let still_current = match capability_context.as_ref() {
                Some(context) => require_account_context(inner, context).await.is_ok(),
                None => true,
            };
            if still_current {
                crate::org_mcp::set_capability_enabled(false);
            }
        }
    }
    result
}

async fn update_bootstrap_with_context_inner(
    inner: &Arc<OrgInner>,
    expected_context: Option<&AccountContext>,
) -> Result<(Value, AccountContext), String> {
    let (data, request_context) = authenticated_json_with_expected_context(
        inner,
        Method::GET,
        "/api/v1/client/bootstrap",
        None,
        expected_context,
    )
    .await?;
    let key_text = data
        .get("signingPublicKey")
        .and_then(Value::as_str)
        .ok_or("bootstrap missing signing public key")?;
    if key_text.len() > MAX_SIGNING_FIELD_BYTES {
        return Err("organization signing public key exceeds the safety limit".into());
    }
    let key = decode_public_key(key_text)?;
    let payload = data
        .get("policyPayload")
        .and_then(Value::as_str)
        .ok_or("bootstrap missing signed policy payload")?;
    if payload.len() > MAX_SIGNING_FIELD_BYTES {
        return Err("organization policy payload exceeds the safety limit".into());
    }
    let signature_text = data
        .get("policySignature")
        .and_then(Value::as_str)
        .ok_or("bootstrap missing policy signature")?;
    if signature_text.len() > MAX_SIGNING_FIELD_BYTES {
        return Err("organization policy signature exceeds the safety limit".into());
    }
    verify_signed_payload(&key, payload, signature_text, "organization policy")?;
    let signed_policy: Value = serde_json::from_str(payload)
        .map_err(|error| format!("decode signed organization policy: {error}"))?;
    if data.get("policy") != Some(&signed_policy) {
        return Err("organization policy payload does not match its signature".into());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let issued_at = signed_policy
        .get("issuedAt")
        .and_then(Value::as_u64)
        .ok_or("organization policy missing issuedAt")?;
    let expires_at = signed_policy
        .get("expiresAt")
        .and_then(Value::as_u64)
        .ok_or("organization policy missing expiresAt")?;
    if expires_at <= now || issued_at > now.saturating_add(5 * 60_000) || expires_at <= issued_at {
        return Err("organization policy is expired or has an invalid validity window".into());
    }
    let next_version = signed_policy
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("organization policy missing version")?;
    let mut session = inner.session.lock().await;
    require_account_context_locked(inner, &session, &request_context)?;
    let profile = session
        .profile
        .as_ref()
        .ok_or("organization profile missing")?;
    let key_account = signing_key_account(profile);
    match credential_read(&key_account)? {
        Some(pinned) if pinned != key_text => {
            return Err(
                "organization signing key changed; explicit administrator migration is required"
                    .into(),
            );
        }
        None => credential_write(&key_account, key_text)?,
        Some(_) => {}
    }
    let prior_version = session
        .bootstrap
        .as_ref()
        .and_then(|value| value.get("policy"))
        .and_then(|value| value.get("version"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if next_version < prior_version {
        return Err("organization policy rollback was rejected".into());
    }
    if let Some(user) = data.get("user") {
        ensure_value_size(user, 1024 * 1024, "organization user profile")?;
    }
    ensure_value_size(
        &data,
        MAX_JSON_RESPONSE_BYTES as usize,
        "organization bootstrap",
    )?;
    session.user = data.get("user").cloned();
    session.bootstrap = Some(data.clone());
    Ok((data, request_context))
}

fn bootstrap_enables_organization_memory(bootstrap: &Value) -> bool {
    bootstrap
        .get("scopes")
        .and_then(Value::as_array)
        .is_some_and(|scopes| {
            scopes.iter().any(|scope| {
                matches!(
                    scope.get("kind").and_then(Value::as_str),
                    Some("team" | "org")
                )
            })
        })
}

fn is_organization_capability_failure(error: &str) -> bool {
    [
        "organization server unreachable",
        "organization server unavailable",
        "read organization response",
        "decode server response",
        "refresh organization session",
        "organization session expired",
        "not signed in to an organization",
        "organization session has no usable credential",
        "organization account changed",
        "knowledge ask: HTTP 5",
        "read knowledge answer",
        "knowledge answer is not valid UTF-8",
        "decode knowledge final event",
        "knowledge answer stream ended without a final event",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

async fn require_policy(inner: &Arc<OrgInner>, key: &str) -> Result<AccountContext, String> {
    let (bootstrap, request_context) = update_bootstrap_with_context(inner, None).await?;
    if bootstrap
        .get("policy")
        .and_then(|policy| policy.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        Ok(request_context)
    } else {
        Err(format!("organization policy disables {key}"))
    }
}

fn session_view(session: &OrgSession) -> OrgSessionView {
    let server_url = session
        .profile
        .as_ref()
        .map(|profile| profile.server_url.clone())
        .or_else(|| {
            session
                .login_hint
                .as_ref()
                .map(|hint| hint.server_url.clone())
        });
    let username = session
        .profile
        .as_ref()
        .map(|profile| profile.username.clone())
        .or_else(|| {
            session
                .login_hint
                .as_ref()
                .map(|hint| hint.username.clone())
        });
    OrgSessionView {
        logged_in: session_has_credential(session),
        organization_memory_enabled: crate::org_mcp::capability_enabled(),
        server_url,
        username,
        requires_reauthentication: session.requires_reauthentication,
        user: session.user.clone(),
        bootstrap: session.bootstrap.clone(),
    }
}

#[tauri::command]
pub async fn org_login(
    app: AppHandle,
    state: State<'_, OrgState>,
    server_url: String,
    username: String,
    password: String,
) -> Result<OrgSessionView, String> {
    let username = username.trim().to_string();
    if username.is_empty()
        || username.chars().count() > MAX_USERNAME_CHARS
        || username.chars().any(char::is_control)
    {
        return Err("organization username is invalid or too long".into());
    }
    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        return Err("organization password is empty or exceeds the safety limit".into());
    }
    let server_url = normalize_server_url(&server_url)?;
    let device_id = Uuid::now_v7().to_string();
    let response = state
        .inner
        .client
        .post(endpoint(&server_url, "/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password, "deviceId": device_id }))
        .send()
        .await
        .map_err(|e| format!("organization server unreachable: {e}"))?;
    let data = response_data(response).await?;
    let access_token = data
        .get("accessToken")
        .and_then(Value::as_str)
        .ok_or("login response missing access token")?;
    let refresh_token = data
        .get("refreshToken")
        .and_then(Value::as_str)
        .ok_or("login response missing refresh token")?;
    let access_token = validated_token(access_token, "organization access token")?;
    let refresh_token = validated_token(refresh_token, "organization refresh token")?;
    let user = data
        .get("user")
        .cloned()
        .ok_or("login response missing user")?;
    ensure_value_size(&user, 1024 * 1024, "organization user profile")?;
    let user_id = user
        .get("id")
        .and_then(Value::as_str)
        .ok_or("login response missing user id")?
        .to_string();
    if user_id.trim().is_empty()
        || user_id.chars().count() > MAX_PROFILE_FIELD_CHARS
        || user_id.chars().any(char::is_control)
    {
        return Err("organization user id is invalid or too long".into());
    }
    let profile = OrgProfile {
        server_url,
        username,
        user_id,
        device_id,
    };
    validate_profile(&profile)?;
    let login_hint = login_hint_from_profile(&profile)?;
    let new_credential_account = credential_account(&profile);
    // Do not revoke the active account merely because a replacement login
    // fails (bad password, offline server, malformed response). Switch the
    // local security boundary only after the new identity is authenticated.
    let login_context = {
        let mut session = state.inner.session.lock().await;
        let previous_profile = session.profile.clone();
        // A sync may still be in flight under the previous account. Invalidate
        // it and purge while the session lock prevents a new old-account
        // snapshot, then publish the new identity.
        invalidate_account_generation(&state.inner);
        deactivate_managed_skills(&state.inner)?;
        with_model_state_transaction(&state.inner, || {
            // Any old-model commit either completed before this lock and is
            // removed here, or observes the new account generation after it.
            crate::providers::remove_organization_model_config()?;
            credential_write(&new_credential_account, &refresh_token)?;
            if let Err(error) = write_json_private(&profile_path(), &profile) {
                let cleanup = credential_delete(&new_credential_account);
                return Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup_error) => {
                        format!("{error}; credential cleanup: {cleanup_error}")
                    }
                });
            }
            if let Err(error) = write_json_private(&login_hint_path(), &login_hint) {
                // The authenticated profile contains the same non-secret
                // fields, so login remains usable. A later refresh/logout will
                // retry materializing the independent hint.
                tracing::warn!(%error, "failed to persist organization login hint");
            }
            if let Some(previous_profile) = previous_profile
                .as_ref()
                .filter(|previous| credential_account(previous) != new_credential_account)
            {
                for old_account in [
                    credential_account(previous_profile),
                    signing_key_account(previous_profile),
                ] {
                    if let Err(error) = credential_delete(&old_account) {
                        // The new profile is already durable and must not be
                        // rolled back into an inconsistent old session merely
                        // because best-effort stale credential cleanup failed.
                        tracing::warn!(%error, "failed to delete credential for replaced organization account");
                    }
                }
            }
            Ok(())
        })?;
        *session = OrgSession {
            profile: Some(profile),
            access_token: Some(access_token),
            refresh_token: Some(refresh_token),
            user: Some(user),
            bootstrap: None,
            login_hint: Some(login_hint),
            requires_reauthentication: false,
        };
        AccountContext {
            account_id: new_credential_account.clone(),
            generation: state.inner.account_generation.load(Ordering::SeqCst),
        }
    };
    // The authenticated identity has changed, but its signed bootstrap and
    // shared-scope membership have not been verified yet. Detach the previous
    // account's tools immediately; successful bootstrap below re-enables them.
    crate::org_mcp::set_capability_enabled(false);
    crate::org_mcp::reconcile_all_sessions(&app).await;
    notify_skills_changed(&app, "account-switch");
    notify_models_changed(&app, "account-switch");
    if let Err(error) = update_bootstrap_with_context(&state.inner, Some(&login_context)).await {
        let (failure, superseded) = {
            let mut session = state.inner.session.lock().await;
            if require_account_context_locked(&state.inner, &session, &login_context).is_err() {
                (
                    format!("{error}; organization login was superseded by another account change"),
                    true,
                )
            } else {
                let profile = session.profile.clone();
                let message = match clear_local_session(
                    &state.inner,
                    &mut session,
                    profile.as_ref(),
                    false,
                ) {
                    Ok(()) => error,
                    Err(cleanup_error) => {
                        format!("{error}; local session cleanup: {cleanup_error}")
                    }
                };
                (message, false)
            }
        };
        if !superseded {
            crate::org_mcp::reconcile_all_sessions(&app).await;
            notify_session_changed(&app, "login-bootstrap-failed").await;
        }
        return Err(failure);
    }
    crate::org_mcp::reconcile_all_sessions(&app).await;
    notify_session_changed(&app, "login").await;
    // 模型凭证优先同步，避免受管 Skill 包下载延迟模型进入 Runtime。
    match sync_organization_model_config_for_context(&state.inner, Some(&login_context)).await {
        Ok(Some(model_id)) => {
            tracing::info!(%model_id, "organization model configuration downloaded");
            notify_models_changed(&app, "login-sync");
        }
        Ok(None) => tracing::debug!("organization chat model is not configured"),
        Err(error) => {
            notify_models_changed(&app, "login-sync-error");
            tracing::warn!(%error, "initial organization model sync failed");
        }
    }
    // 登录即同步。失败不退出账号，但受管 Skill 保持已停用，
    // 避免网络短暂抖动迫使用户重新输入密码。
    match sync_skills_for_context(&state.inner, Some(&login_context)).await {
        Ok(_) => notify_skills_changed(&app, "login-sync"),
        Err(error) => tracing::warn!(%error, "initial managed Skill sync failed"),
    }
    let session = state.inner.session.lock().await;
    require_account_context_locked(&state.inner, &session, &login_context)?;
    Ok(session_view(&session))
}

#[tauri::command]
pub async fn org_logout(app: AppHandle, state: State<'_, OrgState>) -> Result<(), String> {
    crate::org_mcp::set_capability_enabled(false);
    // Local logout is the security boundary and must not wait for an offline
    // organization server. Revoke the remote device token afterwards with a
    // short timeout when an access token is available.
    let (profile, access_token, session_cleanup) = {
        let mut session = state.inner.session.lock().await;
        let current_profile = session.profile.clone();
        let access_token = session.access_token.clone();
        let cleanup =
            clear_local_session(&state.inner, &mut session, current_profile.as_ref(), false);
        (current_profile, access_token, cleanup)
    };
    crate::org_mcp::reconcile_all_sessions(&app).await;
    notify_session_changed(&app, "logout").await;
    notify_skills_changed(&app, "logout");
    notify_models_changed(&app, "logout");
    if let (Some(profile), Some(access_token)) = (&profile, access_token) {
        let _ = state
            .inner
            .client
            .post(endpoint(&profile.server_url, "/api/v1/auth/logout"))
            .bearer_auth(access_token)
            .json(&json!({}))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    }
    session_cleanup
}

#[tauri::command]
pub async fn org_session(
    app: AppHandle,
    state: State<'_, OrgState>,
) -> Result<OrgSessionView, String> {
    let has_credential = {
        let session = state.inner.session.lock().await;
        session_has_credential(&session)
    };
    if has_credential {
        if update_bootstrap(&state.inner).await.is_ok() {
            match sync_organization_model_config(&state.inner).await {
                Ok(Some(model_id)) => {
                    tracing::info!(%model_id, "organization model configuration restored");
                    notify_models_changed(&app, "session-restore");
                }
                Ok(None) => notify_models_changed(&app, "session-restore-unconfigured"),
                Err(error) => {
                    notify_models_changed(&app, "session-restore-error");
                    tracing::warn!(%error, "organization model restore failed");
                }
            }
            let _ = sync_skills(&state.inner).await;
        } else {
            enforce_skill_lease();
            let removed = enforce_model_for_current_session(&state.inner).await;
            match removed {
                Ok(true) => notify_models_changed(&app, "lease-expired"),
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "failed to enforce organization model lease"),
            }
        }
        notify_skills_changed(&app, "session-restore");
    } else {
        crate::org_mcp::set_capability_enabled(false);
    }
    crate::org_mcp::reconcile_all_sessions(&app).await;
    let session = state.inner.session.lock().await;
    Ok(session_view(&session))
}

#[tauri::command]
pub async fn org_sync_model_config(
    app: AppHandle,
    state: State<'_, OrgState>,
) -> Result<OrgModelSyncView, String> {
    if let Err(error) = update_bootstrap(&state.inner).await {
        if enforce_model_for_current_session(&state.inner).await? {
            notify_models_changed(&app, "lease-expired");
        }
        return Err(error);
    }
    let result = sync_organization_model_config(&state.inner).await;
    notify_models_changed(&app, "manual-sync");
    let model_id = result?;
    let synced_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    Ok(OrgModelSyncView {
        configured: model_id.is_some(),
        model_id,
        synced_at,
    })
}

#[tauri::command]
pub async fn org_bootstrap(state: State<'_, OrgState>) -> Result<Value, String> {
    update_bootstrap(&state.inner).await
}

#[tauri::command]
pub async fn org_list_scopes(state: State<'_, OrgState>) -> Result<Value, String> {
    let bootstrap = update_bootstrap(&state.inner).await?;
    Ok(bootstrap
        .get("scopes")
        .cloned()
        .unwrap_or_else(|| json!([])))
}

#[tauri::command]
pub async fn org_list_documents(
    state: State<'_, OrgState>,
    scope_id: Option<String>,
    query: Option<String>,
    page: Option<u32>,
    size: Option<u32>,
) -> Result<Value, String> {
    if let Some(scope_id) = scope_id.as_deref() {
        validate_resource_id(scope_id, "organization scope id")?;
    }
    if let Some(query) = query.as_deref() {
        validate_bounded_text(query, "organization document query", MAX_QUERY_CHARS, true)?;
    }
    let mut path = Url::parse("http://local/api/v1/docs").unwrap();
    {
        let mut pairs = path.query_pairs_mut();
        if let Some(scope) = scope_id {
            pairs.append_pair("scopeId", &scope);
        }
        if let Some(query) = query {
            pairs.append_pair("q", &query);
        }
        pairs.append_pair("page", &page.unwrap_or(1).max(1).to_string());
        pairs.append_pair("size", &size.unwrap_or(20).clamp(1, 100).to_string());
    }
    let request_path = format!(
        "{}{}",
        path.path(),
        path.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    authenticated_json(&state.inner, Method::GET, &request_path, None).await
}

fn library_query_path(
    resource: &str,
    scope_id: Option<&str>,
    query: Option<&str>,
    filter: Option<&str>,
    page: Option<u32>,
    size: Option<u32>,
) -> Result<String, String> {
    if !matches!(
        resource,
        "summary"
            | "memories"
            | "documents"
            | "skills"
            | "memory-submissions"
            | "document-submissions"
            | "skill-submissions"
    ) {
        return Err("organization library resource is invalid".into());
    }
    if let Some(scope) = scope_id.filter(|s| !s.is_empty()) {
        validate_resource_id(scope, "organization scope id")?;
    }
    if let Some(query) = query {
        validate_bounded_text(query, "organization library query", MAX_QUERY_CHARS, true)?;
    }
    if let Some(filter) = filter {
        validate_bounded_text(filter, "organization library filter", 30, true)?;
    }
    let mut url = Url::parse("http://local/api/v1/knowledge/library").unwrap();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("resource", resource);
        for (key, value) in [("scopeId", scope_id), ("q", query), ("filter", filter)] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                pairs.append_pair(key, value);
            }
        }
        pairs.append_pair("page", &page.unwrap_or(1).clamp(1, 1_000_000).to_string());
        pairs.append_pair("size", &size.unwrap_or(20).clamp(1, 100).to_string());
    }
    Ok(format!(
        "{}?{}",
        url.path(),
        url.query().unwrap_or_default()
    ))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn org_library_query(
    state: State<'_, OrgState>,
    resource: String,
    scope_id: Option<String>,
    query: Option<String>,
    filter: Option<String>,
    page: Option<u32>,
    size: Option<u32>,
) -> Result<Value, String> {
    let path = library_query_path(
        &resource,
        scope_id.as_deref(),
        query.as_deref(),
        filter.as_deref(),
        page,
        size,
    )?;
    authenticated_json(&state.inner, Method::GET, &path, None).await
}

#[tauri::command]
pub async fn org_list_memories(
    state: State<'_, OrgState>,
    scope_id: Option<String>,
    kind: Option<String>,
    query: Option<String>,
) -> Result<Value, String> {
    if let Some(scope_id) = scope_id.as_deref() {
        validate_resource_id(scope_id, "organization scope id")?;
    }
    if let Some(kind) = kind.as_deref() {
        if !matches!(
            kind,
            "fact" | "decision" | "convention" | "pitfall" | "howto"
        ) {
            return Err("organization memory kind is invalid".into());
        }
    }
    if let Some(query) = query.as_deref() {
        validate_bounded_text(query, "organization memory query", MAX_QUERY_CHARS, true)?;
    }
    let mut url = Url::parse("http://local/api/v1/memories").unwrap();
    {
        let mut pairs = url.query_pairs_mut();
        if let Some(scope) = scope_id {
            pairs.append_pair("scope", &scope);
        }
        if let Some(kind) = kind {
            pairs.append_pair("kind", &kind);
        }
        if let Some(query) = query {
            pairs.append_pair("q", &query);
        }
    }
    let request_path = format!(
        "{}{}",
        url.path(),
        url.query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    );
    authenticated_json(&state.inner, Method::GET, &request_path, None).await
}

#[tauri::command]
pub async fn org_memory_promotions_mine(state: State<'_, OrgState>) -> Result<Value, String> {
    authenticated_json(&state.inner, Method::GET, "/api/v1/promotions/mine", None).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn org_submit_memory_candidate(
    state: State<'_, OrgState>,
    target_scope_id: String,
    kind: String,
    content: String,
    rationale: Option<String>,
    outcome: Option<String>,
    workspace_ref: Option<String>,
    valid_until: Option<i64>,
    sensitivity: Option<u8>,
) -> Result<Value, String> {
    validate_resource_id(&target_scope_id, "organization target scope id")?;
    if !matches!(
        kind.as_str(),
        "fact" | "decision" | "convention" | "pitfall" | "howto"
    ) {
        return Err("organization memory kind is invalid".into());
    }
    validate_bounded_text(&content, "organization memory content", 2_000, false)?;
    for (value, label, max) in [
        (rationale.as_deref(), "organization memory rationale", 2_000),
        (outcome.as_deref(), "organization memory outcome", 2_000),
        (
            workspace_ref.as_deref(),
            "organization workspace reference",
            1_000,
        ),
    ] {
        if let Some(value) = value {
            validate_bounded_text(value, label, max, true)?;
        }
    }
    let sensitivity = sensitivity.unwrap_or(0);
    if sensitivity > 3 {
        return Err("organization memory sensitivity must be from 0 to 3".into());
    }
    let mut payload = json!({
        "kind": kind,
        "content": content,
        "confidence": 0.7,
        "observedAt": chrono::Utc::now().timestamp_millis(),
        "sensitivity": sensitivity
    });
    if let Some(value) = rationale {
        payload["rationale"] = json!(value);
    }
    if let Some(value) = outcome {
        payload["outcome"] = json!(value);
    }
    if let Some(value) = workspace_ref {
        payload["workspaceRef"] = json!(value);
    }
    if let Some(value) = valid_until {
        payload["validUntil"] = json!(value);
    }
    authenticated_json(
        &state.inner,
        Method::POST,
        "/api/v1/promotions",
        Some(json!({
            "payloadType": "memory",
            "payload": payload,
            "source": "manual",
            "targetScope": target_scope_id
        })),
    )
    .await
}

#[tauri::command]
pub async fn org_document_status(
    state: State<'_, OrgState>,
    doc_id: String,
) -> Result<Value, String> {
    validate_resource_id(&doc_id, "organization document id")?;
    authenticated_json(
        &state.inner,
        Method::GET,
        &format!("/api/v1/docs/{}/status", urlencoding::encode(&doc_id)),
        None,
    )
    .await
}

fn document_fetch_body(doc_id: String, page: Option<u32>) -> Value {
    let mut body = json!({ "docId": doc_id });
    if let Some(page) = page {
        body["page"] = json!(page);
    }
    body
}

#[tauri::command]
pub async fn org_fetch_document(
    state: State<'_, OrgState>,
    doc_id: String,
    page: Option<u32>,
    range: Option<String>,
) -> Result<Value, String> {
    validate_resource_id(&doc_id, "organization document id")?;
    if range.as_ref().is_some_and(|value| {
        value.len() > 32
            || !value
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch == ':' || ch == '-')
    }) {
        return Err("invalid document range".into());
    }
    let mut body = document_fetch_body(doc_id, page);
    if let Some(range) = range {
        body["range"] = json!(range);
    }
    authenticated_json(&state.inner, Method::POST, "/api/v1/docs/fetch", Some(body)).await
}

#[tauri::command]
pub async fn org_document_submissions_mine_page(
    state: State<'_, OrgState>,
    page: Option<u32>,
    size: Option<u32>,
) -> Result<Value, String> {
    let path = format!(
        "/api/v1/document-submissions/mine/page?page={}&size={}",
        page.unwrap_or(1).max(1),
        size.unwrap_or(20).clamp(1, 100)
    );
    authenticated_json(&state.inner, Method::GET, &path, None).await
}

#[tauri::command]
pub async fn org_remove_own_submission(
    state: State<'_, OrgState>,
    submission_id: String,
) -> Result<Value, String> {
    validate_resource_id(&submission_id, "organization submission id")?;
    authenticated_json(
        &state.inner,
        Method::DELETE,
        &format!(
            "/api/v1/document-submissions/{}",
            urlencoding::encode(&submission_id)
        ),
        None,
    )
    .await
}

#[tauri::command]
pub async fn org_preview_document(
    state: State<'_, OrgState>,
    resource_id: String,
    submission: bool,
    source_type: String,
    file_name: String,
) -> Result<Value, String> {
    validate_resource_id(&resource_id, "organization document id")?;
    let path = if submission {
        format!(
            "/api/v1/document-submissions/{}/raw",
            urlencoding::encode(&resource_id)
        )
    } else {
        format!("/api/v1/docs/{}/raw", urlencoding::encode(&resource_id))
    };
    let authenticated = authenticated_response(&state.inner, Method::GET, &path, None).await?;
    if !authenticated.response.status().is_success() {
        return Err(response_data(authenticated.response)
            .await
            .err()
            .unwrap_or_else(|| "无法读取提交原件".into()));
    }
    let bytes = read_response_bounded(
        authenticated.response,
        MAX_DOCUMENT_PREVIEW_BYTES,
        "document preview",
    )
    .await?;
    require_account_context(&state.inner, &authenticated.context).await?;
    if matches!(source_type.as_str(), "md" | "txt") {
        return Ok(json!({ "kind": "text", "content": String::from_utf8_lossy(&bytes) }));
    }
    let lower = file_name.to_ascii_lowercase();
    let mime = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    Ok(json!({
        "kind": "binary",
        "content": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
    }))
}

#[tauri::command]
pub async fn org_download_document(
    app: AppHandle,
    state: State<'_, OrgState>,
    resource_id: String,
    submission: bool,
    suggested_name: String,
) -> Result<Option<String>, String> {
    validate_resource_id(&resource_id, "organization document id")?;
    let suggested_name = suggested_name.replace(['/', '\\'], "_");
    let suggested_name = suggested_name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect::<String>();
    let suggested_name = if suggested_name.is_empty() {
        "document".to_owned()
    } else {
        suggested_name
    };
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("保存组织文档")
        .set_file_name(&suggested_name)
        .save_file(move |selection| {
            let _ = sender.send(selection);
        });
    let Some(selection) = receiver
        .await
        .map_err(|_| "保存对话框意外关闭".to_owned())?
    else {
        return Ok(None);
    };
    let destination = selection
        .into_path()
        .map_err(|error| format!("保存位置无效：{error}"))?;
    let parent = destination
        .parent()
        .ok_or("保存位置缺少目录")?
        .canonicalize()
        .map_err(|error| format!("无法访问保存目录：{error}"))?;
    let file_name = destination.file_name().ok_or("保存文件名无效")?;
    let destination = parent.join(file_name);
    let path = if submission {
        format!(
            "/api/v1/document-submissions/{}/raw",
            urlencoding::encode(&resource_id)
        )
    } else {
        format!("/api/v1/docs/{}/raw", urlencoding::encode(&resource_id))
    };
    let authenticated = authenticated_response(&state.inner, Method::GET, &path, None).await?;
    if !authenticated.response.status().is_success() {
        return Err(response_data(authenticated.response)
            .await
            .err()
            .unwrap_or_else(|| "文档下载失败".into()));
    }
    if authenticated
        .response
        .content_length()
        .is_some_and(|len| len > MAX_DOCUMENT_DOWNLOAD_BYTES)
    {
        return Err("文档超过下载大小上限".into());
    }
    let temporary = parent.join(format!(".echo-download-{}.tmp", Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|error| format!("创建下载文件失败：{error}"))?;
        let mut stream = authenticated.response.bytes_stream();
        let mut total = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("文档下载中断：{error}"))?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_DOCUMENT_DOWNLOAD_BYTES {
                return Err("文档超过下载大小上限".into());
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("写入下载文件失败：{error}"))?;
        }
        file.flush()
            .await
            .map_err(|error| format!("保存下载文件失败：{error}"))?;
        drop(file);
        require_account_context(&state.inner, &authenticated.context).await?;
        tokio::fs::rename(&temporary, &destination)
            .await
            .map_err(|error| format!("完成下载失败：{error}"))?;
        Ok::<(), String>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result?;
    Ok(Some(destination.to_string_lossy().into_owned()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrgFolderFile {
    path: String,
    relative_path: String,
    size: u64,
}

#[tauri::command]
pub async fn org_scan_document_folder(
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
    folder_path: String,
) -> Result<Value, String> {
    let root = filesystem.require_workspace(&folder_path)?;
    tokio::task::spawn_blocking(move || {
        let mut queue = std::collections::VecDeque::from([(root.clone(), 0_usize)]);
        let mut items = Vec::<OrgFolderFile>::new();
        let mut scanned = 0_usize;
        let mut skipped = 0_usize;
        let mut total_size = 0_u64;
        while let Some((directory, depth)) = queue.pop_front() {
            if depth > 8 {
                skipped += 1;
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&directory) else {
                skipped += 1;
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    skipped += 1;
                    continue;
                };
                scanned += 1;
                if scanned > 10_000 {
                    return Err("文件夹条目超过 10000 个，请缩小范围".into());
                }
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    skipped += 1;
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    skipped += 1;
                    continue;
                }
                let Ok(canonical) = path.canonicalize() else {
                    skipped += 1;
                    continue;
                };
                if !canonical.starts_with(&root) {
                    skipped += 1;
                    continue;
                }
                if metadata.is_dir() {
                    queue.push_back((canonical, depth + 1));
                    continue;
                }
                let extension = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !metadata.is_file()
                    || !matches!(
                        extension.as_str(),
                        "md" | "txt" | "pdf" | "docx" | "xlsx" | "pptx" | "png" | "jpg" | "jpeg"
                    )
                {
                    skipped += 1;
                    continue;
                }
                if metadata.len() > MAX_DOCUMENT_UPLOAD_BYTES {
                    skipped += 1;
                    continue;
                }
                let relative_path = canonical
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative_path.chars().count() > 500 {
                    skipped += 1;
                    continue;
                }
                total_size = total_size.saturating_add(metadata.len());
                if total_size > 1024 * 1024 * 1024 {
                    return Err("文件夹可上传内容超过 1 GiB".into());
                }
                items.push(OrgFolderFile {
                    path: canonical.to_string_lossy().into_owned(),
                    relative_path,
                    size: metadata.len(),
                });
                if items.len() > 500 {
                    return Err("可上传文件超过 500 个，请分批上传".into());
                }
            }
        }
        items.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        Ok::<Value, String>(json!({ "items": items, "skipped": skipped, "totalBytes": total_size }))
    })
    .await
    .map_err(|error| format!("扫描文件夹失败：{error}"))?
}

#[tauri::command]
pub async fn org_archive_document(
    state: State<'_, OrgState>,
    doc_id: String,
) -> Result<Value, String> {
    validate_resource_id(&doc_id, "organization document id")?;
    authenticated_json(
        &state.inner,
        Method::DELETE,
        &format!("/api/v1/docs/{}", urlencoding::encode(&doc_id)),
        None,
    )
    .await
}

#[tauri::command]
pub async fn org_new_document_version(
    state: State<'_, OrgState>,
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
    doc_id: String,
    file_path: String,
) -> Result<Value, String> {
    validate_resource_id(&doc_id, "organization document id")?;
    if file_path.len() > 4_096 || file_path.contains('\0') {
        return Err("document path is invalid or too long".into());
    }
    let request_context = active_account_context(&state.inner).await?;
    let path = filesystem.require_authorized_file(Path::new(&file_path))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("invalid file name")?
        .to_string();
    let upload = MultipartUpload::File {
        path,
        max_bytes: MAX_DOCUMENT_UPLOAD_BYTES,
        label: "文档",
    };
    authenticated_multipart(
        &state.inner,
        &format!("/api/v1/docs/{}/new-version", urlencoding::encode(&doc_id)),
        &[],
        &name,
        &upload,
        Some(&request_context),
    )
    .await
}

#[tauri::command]
pub async fn org_publish_document(
    state: State<'_, OrgState>,
    doc_id: String,
    target_scope_id: String,
) -> Result<Value, String> {
    validate_resource_id(&doc_id, "organization document id")?;
    validate_resource_id(&target_scope_id, "organization target scope id")?;
    authenticated_json(
        &state.inner,
        Method::POST,
        &format!("/api/v1/docs/{}/publish", urlencoding::encode(&doc_id)),
        Some(json!({ "targetScopeId": target_scope_id })),
    )
    .await
}

#[derive(Clone)]
enum MultipartUpload {
    File {
        path: PathBuf,
        max_bytes: u64,
        label: &'static str,
    },
    Bytes {
        bytes: bytes::Bytes,
        max_bytes: u64,
        label: &'static str,
    },
}

impl MultipartUpload {
    async fn part(&self, file_name: &str) -> Result<reqwest::multipart::Part, String> {
        match self {
            Self::File {
                path,
                max_bytes,
                label,
            } => {
                let metadata = tokio::fs::symlink_metadata(path)
                    .await
                    .map_err(|error| format!("读取{label}失败：{error}"))?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(format!("{label}必须是普通文件"));
                }
                if metadata.len() > *max_bytes {
                    return Err(format!(
                        "{label}超过大小限制（最大 {}MB）",
                        max_bytes / 1024 / 1024
                    ));
                }
                let file = tokio::fs::File::open(path)
                    .await
                    .map_err(|error| format!("打开{label}失败：{error}"))?;
                let limit = *max_bytes;
                let mut streamed = 0_u64;
                let stream = ReaderStream::new(file).map(move |chunk| match chunk {
                    Ok(chunk) => {
                        streamed = streamed.saturating_add(chunk.len() as u64);
                        if streamed > limit {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "upload file grew beyond its size limit",
                            ))
                        } else {
                            Ok(chunk)
                        }
                    }
                    Err(error) => Err(error),
                });
                Ok(
                    reqwest::multipart::Part::stream(reqwest::Body::wrap_stream(stream))
                        .file_name(file_name.to_string()),
                )
            }
            Self::Bytes {
                bytes,
                max_bytes,
                label,
            } => {
                if bytes.len() as u64 > *max_bytes {
                    return Err(format!(
                        "{label}超过大小限制（最大 {}MB）",
                        max_bytes / 1024 / 1024
                    ));
                }
                Ok(
                    reqwest::multipart::Part::stream(reqwest::Body::from(bytes.clone()))
                        .file_name(file_name.to_string()),
                )
            }
        }
    }
}

async fn authenticated_multipart(
    inner: &Arc<OrgInner>,
    path: &str,
    fields: &[(String, String)],
    file_name: &str,
    upload: &MultipartUpload,
    expected_context: Option<&AccountContext>,
) -> Result<Value, String> {
    if path.len() > 8_192
        || !path.starts_with('/')
        || fields.len() > 64
        || file_name.is_empty()
        || file_name.chars().count() > 1_024
        || file_name.chars().any(char::is_control)
    {
        return Err("organization multipart request is invalid or too large".into());
    }
    let mut field_bytes = 0_usize;
    for (key, value) in fields {
        if key.is_empty()
            || key.chars().count() > 128
            || key.chars().any(char::is_control)
            || value.chars().count() > 16 * 1024
            || value.chars().any(|character| character == '\0')
        {
            return Err("organization multipart fields are invalid or too large".into());
        }
        field_bytes = field_bytes
            .checked_add(key.len())
            .and_then(|size| size.checked_add(value.len()))
            .ok_or("organization multipart field size overflow")?;
    }
    if field_bytes > 512 * 1024 {
        return Err("organization multipart fields exceed 512 KiB".into());
    }
    let mut original_context: Option<AccountContext> = None;
    for attempt in 0..2 {
        let (base, access, context) = auth_snapshot(inner).await?;
        if expected_context.is_some_and(|expected| expected != &context) {
            return Err("organization account changed before upload was sent".into());
        }
        if original_context
            .as_ref()
            .is_some_and(|original| original != &context)
        {
            return Err("organization account changed before upload retry".into());
        }
        original_context.get_or_insert_with(|| context.clone());
        if access.is_empty() {
            refresh(inner, "", &context).await?;
            continue;
        }
        let mut form = reqwest::multipart::Form::new();
        for (key, value) in fields {
            form = form.text(key.clone(), value.clone());
        }
        form = form.part("file", upload.part(file_name).await?);
        let response = inner
            .client
            .post(endpoint(&base, path))
            .bearer_auth(&access)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("upload to organization: {e}"))?;
        if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
            refresh(inner, &access, &context).await?;
            continue;
        }
        let data = response_data(response).await?;
        require_account_context(inner, &context).await?;
        return Ok(data);
    }
    Err("organization session expired".into())
}

pub(crate) fn canonical_org_managed_skill_directory(raw: &str) -> Option<PathBuf> {
    let requested = Path::new(raw);
    let directory = if requested.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
        requested.parent()?
    } else {
        requested
    };
    let canonical = directory.canonicalize().ok()?;
    managed_skills_metadata()
        .into_iter()
        .find_map(|(trusted, _)| {
            trusted
                .canonicalize()
                .ok()
                .filter(|trusted| trusted == &canonical)
                .map(|_| canonical.clone())
        })
}

fn require_authorized_skill_upload_source(
    filesystem: &crate::shell_fs::FilesystemAccess,
    raw: &str,
) -> Result<PathBuf, String> {
    if let Some(managed) = crate::skill_installer::canonical_managed_skill_directory(raw) {
        return Ok(managed);
    }
    if let Some(managed) = canonical_org_managed_skill_directory(raw) {
        return Ok(managed);
    }
    let path = Path::new(raw);
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("无法读取 Skill 来源：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("Skill 来源不能是符号链接".into());
    }
    if metadata.is_file() {
        filesystem.require_authorized_file(path)
    } else if metadata.is_dir() {
        filesystem.require_workspace(raw)
    } else {
        Err("Skill 来源必须是普通文件或目录".into())
    }
}

#[tauri::command]
pub async fn org_submit_document(
    state: State<'_, OrgState>,
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
    file_path: String,
    scope_id: String,
    title: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<Value, String> {
    if file_path.len() > 4_096 || file_path.contains('\0') {
        return Err("document path is invalid or too long".into());
    }
    validate_resource_id(&scope_id, "organization scope id")?;
    if let Some(title) = title.as_deref() {
        validate_bounded_text(title, "organization document title", MAX_TITLE_CHARS, true)?;
    }
    if tags.as_ref().is_some_and(|tags| {
        tags.len() > MAX_TAGS
            || tags.iter().any(|tag| {
                tag.trim().is_empty()
                    || tag.chars().count() > MAX_TAG_CHARS
                    || tag.chars().any(char::is_control)
            })
            || tags.iter().map(String::len).sum::<usize>() > 16 * 1024
    }) {
        return Err("organization document tags are invalid or too large".into());
    }
    let (bootstrap, request_context) = update_bootstrap_with_context(&state.inner, None).await?;
    let is_personal = bootstrap
        .get("scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|scope| {
            scope.get("id").and_then(Value::as_str) == Some(scope_id.as_str())
                && scope.get("kind").and_then(Value::as_str) == Some("personal")
        });
    if is_personal
        && !bootstrap
            .get("policy")
            .and_then(|policy| policy.get("allowPersonalCloud"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err("organization policy disables personal cloud knowledge".into());
    }
    let path = filesystem.require_authorized_file(Path::new(&file_path))?;
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("invalid file name")?
        .to_string();
    let upload = MultipartUpload::File {
        path,
        max_bytes: MAX_DOCUMENT_UPLOAD_BYTES,
        label: "文档",
    };
    let mut fields = vec![("scopeId".to_string(), scope_id)];
    if let Some(title) = title {
        fields.push(("title".into(), title));
    }
    if let Some(tags) = tags {
        fields.push(("tags".into(), tags.join(",")));
    }
    authenticated_multipart(
        &state.inner,
        "/api/v1/document-submissions",
        &fields,
        &name,
        &upload,
        Some(&request_context),
    )
    .await
}

#[tauri::command]
pub async fn org_document_submissions_mine(state: State<'_, OrgState>) -> Result<Value, String> {
    authenticated_json(
        &state.inner,
        Method::GET,
        "/api/v1/document-submissions/mine",
        None,
    )
    .await
}

#[tauri::command]
pub async fn org_list_skills(state: State<'_, OrgState>) -> Result<Value, String> {
    authenticated_json(&state.inner, Method::GET, "/api/v1/skills", None).await
}

#[tauri::command]
pub async fn org_skill_detail(
    state: State<'_, OrgState>,
    skill_id: String,
) -> Result<Value, String> {
    validate_resource_id(&skill_id, "organization Skill id")?;
    authenticated_json(
        &state.inner,
        Method::GET,
        &format!("/api/v1/skills/{}", urlencoding::encode(&skill_id)),
        None,
    )
    .await
}

#[tauri::command]
pub async fn org_set_skill_preference(
    app: AppHandle,
    state: State<'_, OrgState>,
    skill_id: String,
    enabled: bool,
) -> Result<Value, String> {
    validate_resource_id(&skill_id, "organization Skill id")?;
    let request_context = active_account_context(&state.inner).await?;
    let request_account = request_context.account_id.clone();
    let result = authenticated_json_for_context(
        &state.inner,
        Method::PUT,
        &format!(
            "/api/v1/skills/{}/preference",
            urlencoding::encode(&skill_id)
        ),
        Some(json!({ "enabled": enabled })),
        &request_context,
    )
    .await?;
    // A preference update and the preceding sync cursor can share the same
    // millisecond. Force a full fetch when installing so the newly enabled
    // package cannot be skipped by the server's strict `updated_at > cursor`.
    invalidate_skill_sync(&state.inner);
    with_skill_state_transaction(&state.inner, || {
        require_account_context_generation(&state.inner, &request_context)?;
        let persisted_profile = read_json::<OrgProfile>(&profile_path())?;
        validate_profile(&persisted_profile)?;
        if credential_account(&persisted_profile) != request_account {
            return Err("organization account changed while updating Skill preference".into());
        }
        let mut local = load_or_recover_skill_state_unlocked()?;
        if !local.account_id.is_empty() && local.account_id != request_account {
            return Err("managed Skill state belongs to a different account".into());
        }
        local.account_id = request_account.clone();
        if enabled {
            local.cursor.clear();
        }
        bump_skill_revision(&mut local);
        write_json_private(&skill_state_path(), &local)
    })?;
    sync_skills_for_context(&state.inner, Some(&request_context)).await?;
    notify_skills_changed(&app, "preference");
    Ok(result)
}

#[tauri::command]
pub async fn org_publish_skill(
    state: State<'_, OrgState>,
    skill_id: String,
    target_scope_id: String,
) -> Result<Value, String> {
    validate_resource_id(&skill_id, "organization Skill id")?;
    validate_resource_id(&target_scope_id, "organization target scope id")?;
    let request_context = require_policy(&state.inner, "allowSkillSubmission").await?;
    authenticated_json_for_context(
        &state.inner,
        Method::POST,
        &format!("/api/v1/skills/{}/publish", urlencoding::encode(&skill_id)),
        Some(json!({ "targetScopeId": target_scope_id })),
        &request_context,
    )
    .await
}

#[tauri::command]
pub async fn org_qa_feedback(
    state: State<'_, OrgState>,
    qa_event_id: String,
    feedback: String,
) -> Result<Value, String> {
    validate_resource_id(&qa_event_id, "organization QA event id")?;
    validate_bounded_text(
        &feedback,
        "organization QA feedback",
        MAX_FEEDBACK_CHARS,
        false,
    )?;
    authenticated_json(
        &state.inner,
        Method::POST,
        &format!(
            "/api/v1/qa-events/{}/feedback",
            urlencoding::encode(&qa_event_id)
        ),
        Some(json!({ "feedback": feedback })),
    )
    .await
}

#[tauri::command]
pub async fn org_submit_skill(
    state: State<'_, OrgState>,
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
    file_path: String,
    scope_id: String,
    version: Option<String>,
) -> Result<Value, String> {
    if file_path.len() > 4_096 || file_path.contains('\0') {
        return Err("Skill path is invalid or too long".into());
    }
    validate_resource_id(&scope_id, "organization scope id")?;
    if let Some(version) = version.as_deref() {
        validate_bounded_text(version, "organization Skill version", 256, false)?;
    }
    let request_context = require_policy(&state.inner, "allowSkillSubmission").await?;
    let path = require_authorized_skill_upload_source(&filesystem, &file_path)?;
    let is_zip = path.is_file()
        && path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("zip"));
    let (name, upload) = if is_zip {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("invalid file name")?
            .to_string();
        (
            name,
            MultipartUpload::File {
                path,
                max_bytes: MAX_SKILL_UPLOAD_BYTES,
                label: "Skill ZIP",
            },
        )
    } else {
        let source = path.to_string_lossy().into_owned();
        let (name, bytes) = tauri::async_runtime::spawn_blocking(move || {
            crate::skill_installer::package_skill_for_upload(&source)
        })
        .await
        .map_err(|error| format!("打包 Skill 失败：{error}"))??;
        (
            name,
            MultipartUpload::Bytes {
                bytes: bytes::Bytes::from(bytes),
                max_bytes: MAX_SKILL_UPLOAD_BYTES,
                label: "Skill ZIP",
            },
        )
    };
    let mut fields = vec![("scopeId".to_string(), scope_id)];
    if let Some(version) = version {
        fields.push(("version".into(), version));
    }
    authenticated_multipart(
        &state.inner,
        "/api/v1/skill-submissions",
        &fields,
        &name,
        &upload,
        Some(&request_context),
    )
    .await
}

#[tauri::command]
pub async fn org_skill_submissions_mine(state: State<'_, OrgState>) -> Result<Value, String> {
    authenticated_json(
        &state.inner,
        Method::GET,
        "/api/v1/skill-submissions/mine",
        None,
    )
    .await
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct InstalledSkill {
    skill_id: String,
    version_id: String,
    version: String,
    name: String,
    path: String,
    package_path: String,
    hash: String,
    signature_payload: String,
    signature: String,
    scope_kind: String,
    mandatory: bool,
    allow_personal_override: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct SkillSyncState {
    /// Stable hash of the authenticated server/user pair. It prevents a state
    /// downloaded for one account being adopted by another account that uses
    /// the same organization signing key.
    account_id: String,
    /// A fresh UUID on every committed mutation. Comparing it at commit time
    /// prevents cross-process ABA/lost-update races.
    revision: String,
    cursor: String,
    lease_until: u64,
    installed: HashMap<String, InstalledSkill>,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedSkillMetadata {
    pub skill_id: String,
    pub version_id: String,
    pub version: String,
    pub scope_kind: String,
    pub mandatory: bool,
    pub allow_personal_override: bool,
}

/// Return trusted organization metadata keyed by each installed directory.
/// Validation reads every signed package, so callers obtain the complete map
/// once and reuse it while annotating a Runtime skill listing.
pub(crate) fn managed_skills_metadata() -> Vec<(PathBuf, ManagedSkillMetadata)> {
    let Ok(state) = read_json::<SkillSyncState>(&skill_state_path()) else {
        return Vec::new();
    };
    if validate_skill_state(&state).is_err() {
        return Vec::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if state.lease_until > 0 && state.lease_until < now {
        return Vec::new();
    }
    let Ok(profile) = read_json::<OrgProfile>(&profile_path()) else {
        return Vec::new();
    };
    if validate_profile(&profile).is_err()
        || state.account_id.is_empty()
        || state.account_id != credential_account(&profile)
    {
        return Vec::new();
    }
    state
        .installed
        .values()
        .map(|item| {
            (
                PathBuf::from(&item.path),
                ManagedSkillMetadata {
                    skill_id: item.skill_id.clone(),
                    version_id: item.version_id.clone(),
                    version: item.version.clone(),
                    scope_kind: item.scope_kind.clone(),
                    mandatory: item.mandatory,
                    allow_personal_override: item.allow_personal_override,
                },
            )
        })
        .collect()
}

fn decode_public_key(spki_base64: &str) -> Result<VerifyingKey, String> {
    let der = base64::engine::general_purpose::STANDARD
        .decode(spki_base64)
        .map_err(|e| format!("decode signing public key: {e}"))?;
    if der.len() < 32 {
        return Err("invalid Ed25519 public key".into());
    }
    let raw: [u8; 32] = der[der.len() - 32..]
        .try_into()
        .map_err(|_| "invalid Ed25519 key")?;
    VerifyingKey::from_bytes(&raw).map_err(|e| format!("invalid Ed25519 key: {e}"))
}

fn verify_signed_payload(
    public_key: &VerifyingKey,
    payload: &str,
    signature_text: &str,
    label: &str,
) -> Result<(), String> {
    let signature_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature_text)
        .map_err(|error| format!("decode {label} signature: {error}"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|error| format!("invalid {label} signature: {error}"))?;
    public_key
        .verify(payload.as_bytes(), &signature)
        .map_err(|_| format!("{label} signature verification failed"))
}

fn verify_skill_package(
    public_key: &VerifyingKey,
    item: &Value,
    bytes: &[u8],
) -> Result<(), String> {
    let expected = item
        .get("hash")
        .and_then(Value::as_str)
        .ok_or("skill sync item missing hash")?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err("downloaded Skill package hash mismatch".into());
    }
    let payload = item
        .get("signaturePayload")
        .and_then(Value::as_str)
        .ok_or("skill sync item missing signature payload")?;
    let signature_text = item
        .get("signature")
        .and_then(Value::as_str)
        .ok_or("skill sync item missing signature")?;
    verify_signed_payload(public_key, payload, signature_text, "Skill package")?;
    let signed: Value = serde_json::from_str(payload)
        .map_err(|error| format!("decode signed Skill manifest: {error}"))?;
    let checks = [
        ("skillId", item.get("skillId")),
        ("versionId", item.get("versionId")),
        ("version", item.get("version")),
        ("hash", item.get("hash")),
        ("scopeKind", item.get("scopeKind")),
        ("mandatory", item.get("mandatory")),
        ("allowPersonalOverride", item.get("allowPersonalOverride")),
    ];
    if signed.get("schema").and_then(Value::as_str) != Some("echo-managed-skill/v2") {
        return Err("unsupported signed Skill manifest schema".into());
    }
    for (key, expected) in checks {
        if signed.get(key) != expected {
            return Err(format!(
                "signed Skill field does not match sync item: {key}"
            ));
        }
    }
    Ok(())
}

fn verify_installed_skill(public_key: &VerifyingKey, item: &InstalledSkill) -> Result<(), String> {
    verify_signed_payload(
        public_key,
        &item.signature_payload,
        &item.signature,
        "installed Skill",
    )?;
    let signed: Value = serde_json::from_str(&item.signature_payload)
        .map_err(|error| format!("decode installed Skill manifest: {error}"))?;
    let expected = json!({
        "schema": "echo-managed-skill/v2",
        "skillId": item.skill_id,
        "versionId": item.version_id,
        "version": item.version,
        "hash": item.hash,
        "scopeKind": item.scope_kind,
        "mandatory": item.mandatory,
        "allowPersonalOverride": item.allow_personal_override
    });
    if signed != expected {
        return Err("installed Skill sidecar differs from its signed manifest".into());
    }
    let home = crate::paths::echo_agent_home_dir();
    let skills_root = home.join("skills");
    let managed_root = organization_skills_root();
    let family = managed_root.join(&item.skill_id);
    let canonical = family.join(&item.version_id);
    let packages = managed_root.join(".packages");
    let package_family = packages.join(&item.skill_id);
    for (label, path) in [
        ("Skills root", &skills_root),
        ("organization Skills root", &managed_root),
        ("managed Skill family", &family),
        ("managed Skill version", &canonical),
        ("managed Skill package root", &packages),
        ("managed Skill package family", &package_family),
    ] {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("inspect {label} {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!("{label} must be a real directory"));
        }
    }
    let canonical_home = home
        .canonicalize()
        .map_err(|error| format!("canonicalize EchoAgent home: {error}"))?;
    let canonical_managed = managed_root
        .canonicalize()
        .map_err(|error| format!("canonicalize organization Skills root: {error}"))?;
    if !canonical_managed.starts_with(&canonical_home) {
        return Err("organization Skills root escaped the EchoAgent home".into());
    }
    let entry = canonical.join("SKILL.md");
    let entry_metadata = std::fs::symlink_metadata(&entry)
        .map_err(|error| format!("inspect managed Skill entry: {error}"))?;
    if item.path != canonical.to_string_lossy()
        || entry_metadata.file_type().is_symlink()
        || !entry_metadata.is_file()
    {
        return Err("installed Skill path is not canonical or its entry is missing".into());
    }
    let canonical_package = package_family.join(format!("{}.zip", item.version_id));
    if item.package_path != canonical_package.to_string_lossy() {
        return Err("installed Skill package cache path is not canonical".into());
    }
    let package = read_file_bounded(
        &canonical_package,
        MAX_SKILL_UPLOAD_BYTES,
        "installed Skill package cache",
    )?;
    if format!("{:x}", Sha256::digest(&package)) != item.hash {
        return Err("installed Skill package cache hash mismatch".into());
    }
    verify_extracted_skill(&package, &canonical)?;
    Ok(())
}

fn collect_installed_files(
    root: &Path,
    directory: &Path,
    files: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_MANAGED_SKILL_DEPTH {
        return Err("installed Skill directory nesting is too deep".into());
    }
    for entry in std::fs::read_dir(directory)
        .map_err(|error| format!("read installed Skill directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read installed Skill entry: {error}"))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("read installed Skill file type: {error}"))?;
        if kind.is_symlink() {
            return Err("installed Skill contains a symbolic link".into());
        }
        if kind.is_dir() {
            collect_installed_files(root, &entry.path(), files, depth + 1)?;
        } else if kind.is_file() {
            if files.len() >= MAX_MANAGED_SKILL_FILES {
                return Err(format!(
                    "installed Skill contains more than {MAX_MANAGED_SKILL_FILES} files"
                ));
            }
            let entry_path = entry.path();
            let relative = entry_path
                .strip_prefix(root)
                .map_err(|_| "installed Skill file escaped its root")?
                .to_path_buf();
            files.insert(relative);
        } else {
            return Err("installed Skill contains an unsupported filesystem entry".into());
        }
    }
    Ok(())
}

/// File permissions are not a security boundary for the desktop user. Compare
/// every extracted byte and the complete file set with the immutable signed ZIP
/// before injecting the package into the Agent Runtime.
fn verify_extracted_skill(bytes: &[u8], root: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("open cached Skill ZIP: {error}"))?;
    if archive.len() > MAX_MANAGED_SKILL_FILES {
        return Err(format!(
            "cached Skill ZIP contains more than {MAX_MANAGED_SKILL_FILES} files"
        ));
    }
    let mut expected = HashSet::new();
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("read cached Skill ZIP: {error}"))?;
        if entry.is_dir() {
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("cached Skill ZIP contains a symbolic link".into());
        }
        let relative = entry
            .enclosed_name()
            .ok_or("unsafe path in cached Skill ZIP")?
            .to_path_buf();
        if relative.components().count() > MAX_MANAGED_SKILL_DEPTH {
            return Err("cached Skill ZIP directory nesting is too deep".into());
        }
        total_size = total_size
            .checked_add(entry.size())
            .ok_or("cached Skill ZIP expanded size overflow")?;
        if total_size > MAX_SKILL_UPLOAD_BYTES {
            return Err("cached Skill ZIP expands beyond 20MB".into());
        }
        let installed = read_file_bounded(
            &root.join(&relative),
            MAX_SKILL_UPLOAD_BYTES,
            "installed Skill file",
        )?;
        let mut packaged = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut packaged)
            .map_err(|error| format!("read cached Skill file {}: {error}", relative.display()))?;
        if installed != packaged {
            return Err(format!(
                "installed Skill file was modified: {}",
                relative.display()
            ));
        }
        expected.insert(relative);
    }
    let mut actual = HashSet::new();
    collect_installed_files(root, root, &mut actual, 0)?;
    if actual != expected {
        return Err("installed Skill file set differs from its signed package".into());
    }
    echo_agent_tools::implementations::skills::capability::load_manifest(root)
        .map_err(|error| format!("installed Skill capability manifest is invalid: {error}"))?;
    Ok(())
}

fn cache_skill_package(bytes: &[u8], skill_id: &str, version_id: &str) -> Result<PathBuf, String> {
    Uuid::parse_str(skill_id).map_err(|_| "invalid Skill id from organization server")?;
    Uuid::parse_str(version_id).map_err(|_| "invalid Skill version id from organization server")?;
    ensure_organization_skills_root()?;
    let packages = organization_skills_root().join(".packages");
    ensure_private_directory_without_symlink(&packages, "managed Skill package cache")?;
    let family = packages.join(skill_id);
    ensure_private_directory_without_symlink(&family, "managed Skill package family")?;
    let path = family.join(format!("{version_id}.zip"));
    crate::paths::write_private_file(&path, bytes)?;
    Ok(path)
}

fn extract_skill_package(
    bytes: &[u8],
    skill_id: &str,
    version_id: &str,
) -> Result<PathBuf, String> {
    Uuid::parse_str(skill_id).map_err(|_| "invalid Skill id from organization server")?;
    Uuid::parse_str(version_id).map_err(|_| "invalid Skill version id from organization server")?;
    ensure_organization_skills_root()?;
    let root = organization_skills_root().join(skill_id);
    ensure_private_directory_without_symlink(&root, "managed Skill family")?;
    let final_dir = root.join(version_id);
    match std::fs::symlink_metadata(&final_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!(
                "managed Skill version must be a real directory: {}",
                final_dir.display()
            ));
        }
        Ok(_) => {
            verify_extracted_skill(bytes, &final_dir)?;
            return Ok(final_dir);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect managed Skill version: {error}")),
    }
    let temp = root.join(format!(".{version_id}.{}.tmp", Uuid::now_v7()));
    std::fs::create_dir_all(&temp).map_err(|e| format!("create Skill staging dir: {e}"))?;
    let result = (|| -> Result<(), String> {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("open Skill ZIP: {e}"))?;
        if archive.len() > 200 {
            return Err("Skill ZIP contains too many files".into());
        }
        let mut total = 0u64;
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|e| format!("read Skill ZIP: {e}"))?;
            if file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Skill ZIP must not contain symbolic links".into());
            }
            total = total.saturating_add(file.size());
            if total > 20 * 1024 * 1024 {
                return Err("Skill ZIP expands beyond 20MB".into());
            }
            let relative = file
                .enclosed_name()
                .ok_or("unsafe path in Skill ZIP")?
                .to_path_buf();
            if relative.components().count() > 12 {
                return Err("Skill ZIP directory nesting is too deep".into());
            }
            let target = temp.join(relative);
            if file.is_dir() {
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("create Skill directory: {e}"))?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create Skill directory: {e}"))?;
            }
            let mut output =
                std::fs::File::create(&target).map_err(|e| format!("create Skill file: {e}"))?;
            std::io::copy(&mut file, &mut output)
                .map_err(|e| format!("extract Skill file: {e}"))?;
            output
                .flush()
                .map_err(|e| format!("flush Skill file: {e}"))?;
            let mut permissions = output
                .metadata()
                .map_err(|e| format!("read Skill file permissions: {e}"))?
                .permissions();
            permissions.set_readonly(true);
            output
                .set_permissions(permissions)
                .map_err(|e| format!("protect managed Skill file: {e}"))?;
        }
        // Only the root entry defines the installed Skill. Nested SKILL.md
        // files are legitimate reference material and are kept in the signed
        // package just like any other documentation.
        if !temp.join("SKILL.md").is_file() {
            return Err("Skill ZIP root must contain SKILL.md".into());
        }
        echo_agent_tools::implementations::skills::capability::load_manifest(&temp)
            .map_err(|error| format!("Skill capability manifest is invalid: {error}"))?;
        std::fs::rename(&temp, &final_dir).map_err(|e| format!("activate Skill package: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    result.map(|_| final_dir)
}

fn pinned_signing_key() -> Result<VerifyingKey, String> {
    let profile = read_json::<OrgProfile>(&profile_path())?;
    let text = credential_read(&signing_key_account(&profile))?
        .ok_or("organization signing key is not pinned")?;
    decode_public_key(&text)
}

fn validate_installed_skill_fields(key: &str, item: &InstalledSkill) -> Result<(), String> {
    if key != item.skill_id
        || Uuid::parse_str(key).is_err()
        || Uuid::parse_str(&item.version_id).is_err()
        || item.version.chars().count() > MAX_PROFILE_FIELD_CHARS
        || item.name.trim().is_empty()
        || item.name.chars().count() > MAX_PROFILE_FIELD_CHARS
        || item.path.len() > 4_096
        || item.package_path.len() > 4_096
        || item.hash.len() > 256
        || item.signature_payload.len() > MAX_SIGNING_FIELD_BYTES
        || item.signature.len() > MAX_SIGNING_FIELD_BYTES
        || item.scope_kind.len() > 64
    {
        return Err("managed Skill state contains invalid or oversized fields".into());
    }
    Ok(())
}

fn validate_skill_state(state: &SkillSyncState) -> Result<(), String> {
    if state.account_id.chars().count() > 256
        || state.account_id.chars().any(char::is_control)
        || (!state.revision.is_empty() && Uuid::parse_str(&state.revision).is_err())
    {
        return Err("managed Skill state identity or revision is invalid".into());
    }
    if state.cursor.chars().count() > 4_096 || state.cursor.chars().any(char::is_control) {
        return Err("managed Skill cursor is invalid or too long".into());
    }
    if state.installed.len() > MAX_MANAGED_SKILLS {
        return Err(format!(
            "managed Skill state exceeds {MAX_MANAGED_SKILLS} entries"
        ));
    }
    for (key, item) in &state.installed {
        validate_installed_skill_fields(key, item)?;
    }
    if state.installed.is_empty() {
        return Ok(());
    }
    let key = pinned_signing_key()?;
    for item in state.installed.values() {
        verify_installed_skill(&key, item)?;
    }
    Ok(())
}

fn bump_skill_revision(state: &mut SkillSyncState) {
    state.revision = Uuid::now_v7().to_string();
}

fn read_skill_state_strict_unlocked() -> Result<SkillSyncState, String> {
    let state = read_json_or_default_if_missing::<SkillSyncState>(&skill_state_path())?;
    validate_skill_state(&state)?;
    Ok(state)
}

fn quarantine_skill_state_unlocked() -> Result<Option<PathBuf>, String> {
    let path = skill_state_path();
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect managed Skill state: {error}")),
        Ok(_) => {}
    }
    let parent = path
        .parent()
        .ok_or("managed Skill state path has no parent")?;
    let quarantine = parent.join(format!(".{SKILL_STATE_FILE}.corrupt.{}", Uuid::now_v7()));
    std::fs::rename(&path, &quarantine)
        .map_err(|error| format!("quarantine invalid managed Skill state: {error}"))?;
    Ok(Some(quarantine))
}

fn ensure_organization_skills_root() -> Result<(), String> {
    let home = crate::paths::echo_agent_home_dir();
    std::fs::create_dir_all(&home)
        .map_err(|error| format!("create EchoAgent home for managed Skills: {error}"))?;
    crate::paths::harden_private_dir(&home)?;
    let skills = home.join("skills");
    ensure_private_directory_without_symlink(&skills, "Skills root")?;
    let organization = skills.join("organization");
    ensure_private_directory_without_symlink(&organization, "organization Skills root")?;
    let canonical_home = home
        .canonicalize()
        .map_err(|error| format!("canonicalize EchoAgent home: {error}"))?;
    let canonical_organization = organization
        .canonicalize()
        .map_err(|error| format!("canonicalize organization Skills root: {error}"))?;
    if !canonical_organization.starts_with(&canonical_home) {
        return Err("organization Skills root escaped the EchoAgent home".into());
    }
    Ok(())
}

fn ensure_private_directory_without_symlink(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!(
                "{label} must be a real directory: {}",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path)
                .map_err(|error| format!("create {label} {}: {error}", path.display()))?;
        }
        Err(error) => return Err(format!("inspect {label} {}: {error}", path.display())),
    }
    crate::paths::harden_private_dir(path)
}

fn purge_organization_skills_root_unlocked() -> Result<(), String> {
    let root = organization_skills_root();
    let parent = root
        .parent()
        .ok_or("organization Skills root has no parent")?;
    match std::fs::symlink_metadata(parent) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect Skills root: {error}")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("Skills root must be a real directory".into());
        }
        Ok(_) => {}
    }
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect organization Skills root: {error}")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            std::fs::remove_file(&root)
                .map_err(|error| format!("remove unsafe organization Skills root: {error}"))
        }
        Ok(_) => {
            // Rename first so Runtime lookups stop seeing the old tree before
            // recursive deletion begins. The fixed sibling path stays under a
            // verified, non-symlink Skills directory.
            let retired = parent.join(format!(".organization.retired.{}", Uuid::now_v7()));
            std::fs::rename(&root, &retired)
                .map_err(|error| format!("retire organization Skills root: {error}"))?;
            std::fs::remove_dir_all(&retired)
                .map_err(|error| format!("delete retired organization Skills root: {error}"))
        }
    }
}

fn write_runtime_skill_config(state: &SkillSyncState) -> Result<(), String> {
    // Sidecar 和 config.toml 都属于用户可编辑文件。每次注入 Runtime
    // 前重新验证服务端签名，不信任 sidecar 里的 mandatory/覆盖值。
    validate_skill_state(state)?;
    crate::providers::update_config(|config| {
        let root = config.as_table_mut().ok_or("config root is not a table")?;
        let skills = root
            .entry("skills")
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .ok_or("[skills] is not a table")?;
        // Runtime only understands a flat Server scope, so collapse same-name
        // packages here. Normally personal > team > org. A mandatory/non-
        // overridable enterprise package wins over an unlocked personal package;
        // between protected packages the broader organization policy wins.
        let mut winners = std::collections::BTreeMap::<String, &InstalledSkill>::new();
        for candidate in state.installed.values() {
            winners
                .entry(candidate.name.clone())
                .and_modify(|current| {
                    if skill_precedes(candidate, current) {
                        *current = candidate;
                    }
                })
                .or_insert(candidate);
        }
        let paths = winners
            .values()
            .map(|item| toml::Value::String(item.path.clone()))
            .collect();
        let enforced = winners
            .values()
            .filter(|item| item.mandatory || !item.allow_personal_override)
            .map(|item| toml::Value::String(item.name.clone()))
            .collect();
        skills.insert("server_skill_dirs".into(), toml::Value::Array(paths));
        skills.insert(
            "server_enforced_skill_names".into(),
            toml::Value::Array(enforced),
        );
        Ok(())
    })
}

fn recover_invalid_skill_state_unlocked(load_error: &str) -> Result<SkillSyncState, String> {
    // Fail closed first. Even if quarantine or disk cleanup later fails, the
    // Runtime must stop receiving paths derived from a corrupt sidecar.
    let mut empty = SkillSyncState::default();
    bump_skill_revision(&mut empty);
    let mut cleanup_errors = Vec::new();
    if let Err(error) = write_runtime_skill_config(&empty) {
        cleanup_errors.push(format!("clear Runtime managed Skills: {error}"));
    }
    let quarantined = match quarantine_skill_state_unlocked() {
        Ok(path) => path,
        Err(error) => {
            cleanup_errors.push(error);
            None
        }
    };
    if let Err(error) = purge_organization_skills_root_unlocked() {
        cleanup_errors.push(error);
    }
    // Only create a fresh sidecar after the invalid object was moved away. A
    // directory/symlink that could not be quarantined must not be overwritten.
    if quarantined.is_some() || !skill_state_path().exists() {
        if let Err(error) = write_json_private(&skill_state_path(), &empty) {
            cleanup_errors.push(format!("write reset managed Skill state: {error}"));
        }
    }
    if cleanup_errors.is_empty() {
        tracing::warn!(
            error = %bounded_message(load_error),
            quarantine = ?quarantined,
            "invalid managed Skill state was quarantined and disabled"
        );
        Ok(empty)
    } else {
        Err(format!(
            "managed Skill state is invalid ({load_error}); fail-closed cleanup: {}",
            cleanup_errors.join("; ")
        ))
    }
}

fn load_or_recover_skill_state_unlocked() -> Result<SkillSyncState, String> {
    match read_skill_state_strict_unlocked() {
        Ok(state) => Ok(state),
        Err(error) => recover_invalid_skill_state_unlocked(&error),
    }
}

async fn active_account_context(inner: &OrgInner) -> Result<AccountContext, String> {
    let session = inner.session.lock().await;
    let profile = session
        .profile
        .as_ref()
        .ok_or("not signed in to an organization")?;
    if session.refresh_token.is_none() && session.access_token.is_none() {
        return Err("organization session has no usable credential".into());
    }
    Ok(AccountContext {
        account_id: credential_account(profile),
        generation: inner.account_generation.load(Ordering::SeqCst),
    })
}

async fn active_skill_account_id(inner: &OrgInner) -> Result<String, String> {
    Ok(active_account_context(inner).await?.account_id)
}

fn require_skill_commit_current(
    expected_epoch: u64,
    current_epoch: u64,
    expected_account: &str,
    current_account: &str,
    base: &SkillSyncState,
    current: &SkillSyncState,
) -> Result<(), String> {
    if expected_epoch != current_epoch || expected_account != current_account {
        return Err("organization account changed during Skill synchronization".into());
    }
    if current != base {
        return Err("organization Skill state changed concurrently; please retry".into());
    }
    Ok(())
}

fn skill_precedes(candidate: &InstalledSkill, current: &InstalledSkill) -> bool {
    let candidate_protected = candidate.mandatory || !candidate.allow_personal_override;
    let current_protected = current.mandatory || !current.allow_personal_override;
    if candidate_protected != current_protected {
        return candidate_protected;
    }
    let default_rank = |kind: &str| match kind {
        "personal" => 3,
        "team" => 2,
        "org" => 1,
        _ => 0,
    };
    let protected_rank = |kind: &str| match kind {
        "org" => 3,
        "team" => 2,
        "personal" => 1,
        _ => 0,
    };
    let candidate_rank = if candidate_protected {
        protected_rank(&candidate.scope_kind)
    } else {
        default_rank(&candidate.scope_kind)
    };
    let current_rank = if current_protected {
        protected_rank(&current.scope_kind)
    } else {
        default_rank(&current.scope_kind)
    };
    candidate_rank > current_rank
        || (candidate_rank == current_rank && candidate.skill_id < current.skill_id)
}

fn deactivate_managed_skills(inner: &OrgInner) -> Result<(), String> {
    // Increment before waiting on the state lock. An in-flight sync either
    // finishes an already locked package write (which this cleanup then
    // purges), or observes the new epoch before its next write/commit.
    invalidate_skill_sync(inner);
    with_skill_state_transaction(inner, || {
        let mut errors = Vec::new();
        if let Err(error) = load_or_recover_skill_state_unlocked() {
            errors.push(error);
        }
        let mut empty = SkillSyncState::default();
        bump_skill_revision(&mut empty);
        // Clear Runtime config before filesystem work; logout must fail closed
        // even if a read-only/corrupt package tree cannot be removed.
        if let Err(error) = write_runtime_skill_config(&empty) {
            errors.push(format!("clear Runtime managed Skills: {error}"));
        }
        if let Err(error) = purge_organization_skills_root_unlocked() {
            errors.push(error);
        }
        if let Err(error) = write_json_private(&skill_state_path(), &empty) {
            errors.push(format!("persist disabled managed Skill state: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    })
}

pub fn enforce_skill_lease() {
    let state = shared_state();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if let Err(error) = with_skill_state_transaction(&state.inner, || {
        let mut local = match read_skill_state_strict_unlocked() {
            Ok(state) => state,
            Err(error) => {
                invalidate_skill_sync(&state.inner);
                recover_invalid_skill_state_unlocked(&error)?
            }
        };
        let persisted_account = read_json::<OrgProfile>(&profile_path())
            .ok()
            .filter(|profile| validate_profile(profile).is_ok())
            .map(|profile| credential_account(&profile));
        let account_mismatch = !local.installed.is_empty()
            && match persisted_account.as_deref() {
                None => true,
                Some(account) => !local.account_id.is_empty() && local.account_id != account,
            };
        let expired = local.lease_until > 0 && local.lease_until < now;
        if account_mismatch || expired {
            invalidate_skill_sync(&state.inner);
            local = SkillSyncState::default();
            bump_skill_revision(&mut local);
            // 租约过期后下次联网必须全量同步。保留旧 cursor 会让服务端
            // 返回空增量，导致已暂停的 Skill 永远无法恢复。
            let mut errors = Vec::new();
            if let Err(error) = write_runtime_skill_config(&local) {
                errors.push(format!("clear Runtime managed Skills: {error}"));
            }
            if let Err(error) = purge_organization_skills_root_unlocked() {
                errors.push(error);
            }
            if let Err(error) = write_json_private(&skill_state_path(), &local) {
                errors.push(format!("persist expired managed Skill state: {error}"));
            }
            return if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            };
        }
        let mut changed = false;
        if !local.installed.is_empty() && local.account_id.is_empty() {
            local.account_id = persisted_account.unwrap_or_default();
            changed = true;
        }
        if local.revision.is_empty() {
            bump_skill_revision(&mut local);
            changed = true;
        }
        // 即使租约未过期也覆写 config.toml，这会在每次 Agent 会话
        // 前恢复签名策略，本地手工删除强制项不能持续生效。
        if changed {
            write_json_private(&skill_state_path(), &local)?;
        }
        write_runtime_skill_config(&local)?;
        Ok(())
    }) {
        tracing::warn!(%error, "failed to enforce managed Skill lease state");
    }
}

fn reconcile_visible_skills(
    local: &mut SkillSyncState,
    visible: &HashSet<String>,
) -> Vec<InstalledSkill> {
    let mut removed = Vec::new();
    local.installed.retain(|skill_id, item| {
        if visible.contains(skill_id) {
            true
        } else {
            removed.push(item.clone());
            false
        }
    });
    removed
}

fn remove_installed_package(item: &InstalledSkill) {
    // Sidecar 状态仍当作不可信输入：只删除
    // skills/organization/<uuid>/<uuid> 受管根下的精确版本。
    if Uuid::parse_str(&item.skill_id).is_err() || Uuid::parse_str(&item.version_id).is_err() {
        tracing::warn!(skill_id = %item.skill_id, version_id = %item.version_id, "refused unsafe managed Skill cleanup path");
        return;
    }
    let managed_root = organization_skills_root();
    if !std::fs::symlink_metadata(&managed_root)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        tracing::warn!(path = %managed_root.display(), "refused managed Skill cleanup through an unsafe root");
        return;
    }
    let family_dir = managed_root.join(&item.skill_id);
    if !std::fs::symlink_metadata(&family_dir)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        // Removing a final-component symlink does not touch its target. Never
        // append a version id through an untrusted family directory.
        if std::fs::symlink_metadata(&family_dir)
            .is_ok_and(|metadata| metadata.file_type().is_symlink() || metadata.is_file())
        {
            let _ = std::fs::remove_file(&family_dir);
        }
        tracing::warn!(path = %family_dir.display(), "refused managed Skill cleanup through an unsafe family directory");
        return;
    }
    let version_dir = family_dir.join(&item.version_id);
    match std::fs::symlink_metadata(&version_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if let Err(error) = std::fs::remove_dir_all(&version_dir) {
                tracing::warn!(%error, path = %version_dir.display(), "failed to remove revoked managed Skill package");
            }
        }
        Ok(_) => {
            if let Err(error) = std::fs::remove_file(&version_dir) {
                tracing::warn!(%error, path = %version_dir.display(), "failed to remove unsafe managed Skill version entry");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, path = %version_dir.display(), "failed to inspect revoked managed Skill package")
        }
    }
    let _ = std::fs::remove_dir(&family_dir);
    let packages = managed_root.join(".packages");
    let package_family = packages.join(&item.skill_id);
    if !std::fs::symlink_metadata(&packages)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        || !std::fs::symlink_metadata(&package_family)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        tracing::warn!(path = %package_family.display(), "refused managed Skill cache cleanup through an unsafe directory");
        return;
    }
    let cached = package_family.join(format!("{}.zip", item.version_id));
    if let Err(error) = std::fs::remove_file(&cached) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, path = %cached.display(), "failed to remove revoked managed Skill cache");
        }
    }
}

async fn sync_skills(inner: &Arc<OrgInner>) -> Result<Value, String> {
    sync_skills_for_context(inner, None).await
}

async fn sync_skills_for_context(
    inner: &Arc<OrgInner>,
    expected_context: Option<&AccountContext>,
) -> Result<Value, String> {
    // Only one network sync per process. Logout/account switch intentionally
    // does not wait for this guard; it invalidates `skill_epoch` and takes the
    // short state transaction lock instead.
    let _sync_guard = inner.skill_sync.lock().await;
    let expected_epoch = inner.skill_epoch.load(Ordering::SeqCst);
    let account_context = active_account_context(inner).await?;
    if expected_context.is_some_and(|expected| expected != &account_context) {
        return Err("organization account changed before Skill synchronization".into());
    }
    let account_id = account_context.account_id.clone();
    require_current_skill_epoch(inner, expected_epoch)?;
    require_account_context_generation(inner, &account_context)?;
    let base = with_skill_state_transaction(inner, || {
        require_current_skill_epoch(inner, expected_epoch)?;
        require_account_context_generation(inner, &account_context)?;
        let persisted_profile = read_json::<OrgProfile>(&profile_path())?;
        validate_profile(&persisted_profile)?;
        if credential_account(&persisted_profile) != account_id {
            return Err("organization account changed before Skill synchronization".into());
        }
        let mut state = load_or_recover_skill_state_unlocked()?;
        if !state.account_id.is_empty() && state.account_id != account_id {
            return Err("managed Skill state belongs to a different account".into());
        }
        let mut changed = false;
        if state.account_id.is_empty() {
            state.account_id = account_id.clone();
            changed = true;
        }
        if state.revision.is_empty() {
            bump_skill_revision(&mut state);
            changed = true;
        }
        if changed {
            write_json_private(&skill_state_path(), &state)?;
        }
        Ok(state)
    })?;
    let mut local = base.clone();
    let path = format!(
        "/api/v1/skills/sync?cursor={}",
        urlencoding::encode(&local.cursor)
    );
    let data =
        authenticated_json_for_context(inner, Method::GET, &path, None, &account_context).await?;
    let (bootstrap, _) = update_bootstrap_with_context(inner, Some(&account_context)).await?;
    require_current_skill_epoch(inner, expected_epoch)?;
    require_account_context_generation(inner, &account_context)?;
    let key_text = bootstrap
        .get("signingPublicKey")
        .and_then(Value::as_str)
        .ok_or("bootstrap missing signing public key")?;
    let public_key = decode_public_key(key_text)?;
    let upserts = data
        .get("upserts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if upserts.len() > MAX_MANAGED_SKILLS {
        return Err(format!(
            "organization Skill sync exceeds {MAX_MANAGED_SKILLS} upserts"
        ));
    }
    let mut obsolete_packages = Vec::new();
    for item in upserts {
        ensure_value_size(
            &item,
            MAX_SIGNING_FIELD_BYTES.saturating_mul(2),
            "organization Skill sync item",
        )?;
        let skill_id = item
            .get("skillId")
            .and_then(Value::as_str)
            .ok_or("Skill item missing id")?
            .to_string();
        let version_id = item
            .get("versionId")
            .and_then(Value::as_str)
            .ok_or("Skill item missing version id")?
            .to_string();
        Uuid::parse_str(&skill_id).map_err(|_| "invalid Skill id from organization server")?;
        Uuid::parse_str(&version_id)
            .map_err(|_| "invalid Skill version id from organization server")?;
        if !local.installed.contains_key(&skill_id) && local.installed.len() >= MAX_MANAGED_SKILLS {
            return Err(format!(
                "managed Skill state exceeds {MAX_MANAGED_SKILLS} entries"
            ));
        }
        let package_url = item
            .get("packageUrl")
            .and_then(Value::as_str)
            .ok_or("Skill item missing package URL")?;
        let authenticated = authenticated_response_for_context(
            inner,
            Method::GET,
            package_url,
            None,
            Some(&account_context),
        )
        .await?;
        let response = authenticated.response;
        if !response.status().is_success() {
            return Err(format!("download Skill: HTTP {}", response.status()));
        }
        let bytes =
            read_response_bounded(response, MAX_SKILL_UPLOAD_BYTES, "managed Skill package")
                .await?;
        verify_skill_package(&public_key, &item, &bytes)?;
        require_current_skill_epoch(inner, expected_epoch)?;
        require_account_context_generation(inner, &account_context)?;
        if active_skill_account_id(inner).await? != account_id {
            return Err("organization account changed during Skill download".into());
        }
        // Package paths become executable Runtime inputs. Check both the
        // in-process epoch and cross-process state baseline while holding the
        // same transaction used by logout/preference/lease mutations.
        let (package_path, install_path) = with_skill_state_transaction(inner, || {
            require_current_skill_epoch(inner, expected_epoch)?;
            require_account_context_generation(inner, &account_context)?;
            let current = match read_skill_state_strict_unlocked() {
                Ok(state) => state,
                Err(error) => {
                    invalidate_skill_sync(inner);
                    let cleanup = recover_invalid_skill_state_unlocked(&error);
                    return Err(format!(
                        "managed Skill state became invalid during synchronization: {error}{}",
                        cleanup
                            .err()
                            .map(|cleanup| format!("; fail-closed cleanup: {cleanup}"))
                            .unwrap_or_default()
                    ));
                }
            };
            require_skill_commit_current(
                expected_epoch,
                inner.skill_epoch.load(Ordering::SeqCst),
                &account_id,
                &current.account_id,
                &base,
                &current,
            )?;
            let package_path = cache_skill_package(&bytes, &skill_id, &version_id)?;
            let install_path = extract_skill_package(&bytes, &skill_id, &version_id)?;
            Ok((package_path, install_path))
        })?;
        let installed = InstalledSkill {
            skill_id: skill_id.clone(),
            version_id: version_id.clone(),
            version: item
                .get("version")
                .and_then(Value::as_str)
                .ok_or("Skill item missing version")?
                .to_string(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&skill_id)
                .to_string(),
            path: install_path.to_string_lossy().into_owned(),
            package_path: package_path.to_string_lossy().into_owned(),
            hash: item
                .get("hash")
                .and_then(Value::as_str)
                .ok_or("Skill item missing hash")?
                .to_string(),
            signature_payload: item
                .get("signaturePayload")
                .and_then(Value::as_str)
                .ok_or("Skill item missing signature payload")?
                .to_string(),
            signature: item
                .get("signature")
                .and_then(Value::as_str)
                .ok_or("Skill item missing signature")?
                .to_string(),
            scope_kind: item
                .get("scopeKind")
                .and_then(Value::as_str)
                .unwrap_or("org")
                .to_string(),
            mandatory: item
                .get("mandatory")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            allow_personal_override: item
                .get("allowPersonalOverride")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        };
        validate_installed_skill_fields(&skill_id, &installed)?;
        if let Some(previous) = local.installed.insert(skill_id, installed) {
            if previous.version_id != version_id {
                obsolete_packages.push(previous);
            }
        }
    }
    let revoked = data
        .get("revoked")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if revoked.len() > MAX_MANAGED_SKILLS {
        return Err(format!(
            "organization Skill sync exceeds {MAX_MANAGED_SKILLS} revocations"
        ));
    }
    for revoked in revoked {
        if let Some(skill_id) = revoked.get("skillId").and_then(Value::as_str) {
            if let Some(previous) = local.installed.remove(skill_id) {
                obsolete_packages.push(previous);
            }
        }
    }
    if let Some(visible_ids) = data.get("visibleSkillIds").and_then(Value::as_array) {
        if visible_ids.len() > MAX_MANAGED_SKILLS {
            return Err(format!(
                "organization Skill sync exceeds {MAX_MANAGED_SKILLS} visible ids"
            ));
        }
        let visible = visible_ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        obsolete_packages.extend(reconcile_visible_skills(&mut local, &visible));
    }
    let next_cursor = data
        .get("nextCursor")
        .and_then(Value::as_str)
        .unwrap_or(&local.cursor);
    if next_cursor.chars().count() > 4_096 || next_cursor.chars().any(char::is_control) {
        return Err("organization Skill cursor is invalid or too long".into());
    }
    local.cursor = next_cursor.to_string();
    local.lease_until = data.get("leaseUntil").and_then(Value::as_u64).unwrap_or(0);
    if active_skill_account_id(inner).await? != account_id {
        return Err("organization account changed before Skill commit".into());
    }
    with_skill_state_transaction(inner, || {
        require_current_skill_epoch(inner, expected_epoch)?;
        require_account_context_generation(inner, &account_context)?;
        let persisted_profile = read_json::<OrgProfile>(&profile_path())?;
        validate_profile(&persisted_profile)?;
        if credential_account(&persisted_profile) != account_id {
            return Err("organization account changed before Skill commit".into());
        }
        let current = match read_skill_state_strict_unlocked() {
            Ok(state) => state,
            Err(error) => {
                invalidate_skill_sync(inner);
                let cleanup = recover_invalid_skill_state_unlocked(&error);
                return Err(format!(
                    "managed Skill state became invalid before commit: {error}{}",
                    cleanup
                        .err()
                        .map(|cleanup| format!("; fail-closed cleanup: {cleanup}"))
                        .unwrap_or_default()
                ));
            }
        };
        require_skill_commit_current(
            expected_epoch,
            inner.skill_epoch.load(Ordering::SeqCst),
            &account_id,
            &current.account_id,
            &base,
            &current,
        )?;
        local.account_id = account_id.clone();
        bump_skill_revision(&mut local);
        // State first, Runtime second: a config write failure cannot leave
        // uncommitted paths active, and the next lease check can safely retry
        // materializing a valid committed state.
        write_json_private(&skill_state_path(), &local)?;
        write_runtime_skill_config(&local)?;
        for item in &obsolete_packages {
            remove_installed_package(item);
        }
        Ok(())
    })?;
    Ok(json!({
        "cursor": local.cursor,
        "leaseUntil": local.lease_until,
        "installed": local.installed.values().collect::<Vec<_>>()
    }))
}

#[tauri::command]
pub async fn org_sync_skills(app: AppHandle, state: State<'_, OrgState>) -> Result<Value, String> {
    let result = sync_skills(&state.inner).await;
    notify_skills_changed(&app, "manual-sync");
    result
}

/// 应用启动后立即尝试恢复会话并同步，之后定时轮询。
/// 无登录/离线是正常状态，只记录 debug；租约到期由独立门禁处理。
pub fn start_background_sync(app: AppHandle) {
    let state = shared_state();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
        loop {
            interval.tick().await;
            let has_credential = {
                let session = state.inner.session.lock().await;
                session_has_credential(&session)
            };
            if !has_credential {
                continue;
            }
            if let Err(error) = update_bootstrap(&state.inner).await {
                tracing::debug!(%error, "organization bootstrap refresh skipped");
                crate::org_mcp::reconcile_all_sessions(&app).await;
                notify_session_changed(&app, "background-unavailable").await;
                enforce_skill_lease();
                notify_skills_changed(&app, "lease-check");
                let removed = enforce_model_for_current_session(&state.inner).await;
                match removed {
                    Ok(true) => notify_models_changed(&app, "lease-expired"),
                    Ok(false) => {}
                    Err(cleanup_error) => {
                        tracing::warn!(%cleanup_error, "failed to enforce organization model lease")
                    }
                }
                continue;
            }
            crate::org_mcp::reconcile_all_sessions(&app).await;
            notify_session_changed(&app, "background-restored").await;
            match sync_organization_model_config(&state.inner).await {
                Ok(_) => notify_models_changed(&app, "background-sync"),
                Err(error) => {
                    notify_models_changed(&app, "background-sync-error");
                    tracing::warn!(%error, "periodic organization model sync failed");
                }
            }
            if let Err(error) = sync_skills(&state.inner).await {
                tracing::warn!(%error, "periodic managed Skill sync failed");
                enforce_skill_lease();
                notify_skills_changed(&app, "lease-check");
            } else {
                notify_skills_changed(&app, "background-sync");
            }
        }
    });
}

fn sse_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|window| window == b"\n\n");
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(index), None) => Some((index, 2)),
        (None, Some(index)) => Some((index, 4)),
        (None, None) => None,
    }
}

fn parse_sse_block(block: &[u8]) -> Result<(String, Value), String> {
    if block.len() > MAX_SSE_EVENT_BYTES {
        return Err("knowledge answer event exceeds 2 MiB".into());
    }
    let text = std::str::from_utf8(block).map_err(|e| format!("decode answer stream: {e}"))?;
    let mut event = "message";
    let mut data = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(value) = line.strip_prefix("event:") {
            event = value.strip_prefix(' ').unwrap_or(value);
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    let data = data.join("\n");
    let payload = serde_json::from_str::<Value>(&data).unwrap_or(Value::String(data));
    Ok((event.to_string(), payload))
}

fn drain_sse_events(pending: &mut Vec<u8>) -> Result<Vec<(String, Value)>, String> {
    let mut events = Vec::new();
    while let Some((index, separator_len)) = sse_boundary(pending) {
        let block = pending[..index].to_vec();
        pending.drain(..index + separator_len);
        if !block.iter().all(u8::is_ascii_whitespace) {
            events.push(parse_sse_block(&block)?);
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_queries_are_bounded_and_cannot_change_the_endpoint() {
        assert!(library_query_path("../auth/logout", None, None, None, None, None).is_err());
        let path = library_query_path(
            "memories",
            Some("team"),
            Some("回滚 & scopeId=private"),
            Some("howto"),
            Some(0),
            Some(500),
        )
        .unwrap();
        let url = Url::parse(&format!("http://local{path}")).unwrap();
        let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.path(), "/api/v1/knowledge/library");
        assert_eq!(pairs["scopeId"], "team");
        assert_eq!(pairs["q"], "回滚 & scopeId=private");
        assert_eq!(pairs["page"], "1");
        assert_eq!(pairs["size"], "100");
        assert!(library_query_path(
            "skills",
            None,
            Some(&"x".repeat(MAX_QUERY_CHARS + 1)),
            None,
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn local_knowledge_snapshot_distinguishes_revoked_sources_from_no_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-knowledge-sources.json");
        assert_eq!(
            read_local_kb_sources_at(&path, true).unwrap(),
            json!({ "exists": false, "sources": [] })
        );
        std::fs::write(&path, "[]").unwrap();
        assert_eq!(
            read_local_kb_sources_at(&path, true).unwrap(),
            json!({ "exists": true, "sources": [] })
        );
        assert_eq!(read_local_kb_sources_at(&path, false).unwrap(), json!([]));
    }

    #[test]
    fn server_url_requires_tls_except_loopback() {
        assert!(normalize_server_url("https://memory.example.com/").is_ok());
        assert!(normalize_server_url("https://10.132.19.82:8787/").is_ok());
        assert!(normalize_server_url("http://127.0.0.1:8787").is_ok());
        assert!(normalize_server_url("http://memory.example.com").is_err());
        assert!(normalize_server_url("https://u:p@memory.example.com").is_err());
        assert!(normalize_server_url(&format!(
            "https://example.com/{}",
            "a".repeat(MAX_SERVER_URL_CHARS)
        ))
        .is_err());
    }

    #[test]
    fn organization_credentials_and_profile_fields_are_bounded() {
        assert!(validated_token("valid-token", "token").is_ok());
        assert!(validated_token(&"x".repeat(MAX_TOKEN_BYTES + 1), "token").is_err());

        let valid = OrgProfile {
            server_url: "https://memory.example.com".into(),
            username: "alice".into(),
            user_id: "user-1".into(),
            device_id: "device-1".into(),
        };
        assert!(validate_profile(&valid).is_ok());
        let mut invalid = valid;
        invalid.username = "x".repeat(MAX_PROFILE_FIELD_CHARS + 1);
        assert!(validate_profile(&invalid).is_err());
    }

    #[test]
    fn login_hint_round_trip_keeps_only_non_secret_identity_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(LOGIN_HINT_FILE);
        let hint = OrgLoginHint {
            server_url: "https://memory.example.com".into(),
            username: "alice".into(),
        };

        validate_login_hint(&hint).unwrap();
        write_json_private(&path, &hint).unwrap();
        let restored = read_optional_json::<OrgLoginHint>(&path).unwrap();

        assert_eq!(restored, Some(hint));
        let serialized = std::fs::read_to_string(path).unwrap();
        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("token"));
    }

    #[test]
    fn signed_out_session_returns_prefill_and_reauthentication_state() {
        let session = OrgSession {
            login_hint: Some(OrgLoginHint {
                server_url: "https://memory.example.com".into(),
                username: "alice".into(),
            }),
            requires_reauthentication: true,
            ..Default::default()
        };

        let view = session_view(&session);

        assert!(!view.logged_in);
        assert_eq!(
            view.server_url.as_deref(),
            Some("https://memory.example.com")
        );
        assert_eq!(view.username.as_deref(), Some("alice"));
        assert!(view.requires_reauthentication);
        assert!(view.user.is_none());
        assert!(view.bootstrap.is_none());
    }

    #[test]
    fn organization_memory_requires_a_shared_scope() {
        assert!(!bootstrap_enables_organization_memory(
            &json!({ "scopes": [] })
        ));
        assert!(!bootstrap_enables_organization_memory(&json!({
            "scopes": [{ "id": "personal-1", "kind": "personal" }]
        })));
        assert!(bootstrap_enables_organization_memory(&json!({
            "scopes": [{ "id": "team-1", "kind": "team" }]
        })));
        assert!(bootstrap_enables_organization_memory(&json!({
            "scopes": [{ "id": "org-1", "kind": "org" }]
        })));
    }

    #[test]
    fn only_connectivity_and_auth_failures_trip_the_optional_capability() {
        assert!(is_organization_capability_failure(
            "organization server unreachable: offline"
        ));
        assert!(is_organization_capability_failure(
            "organization session expired"
        ));
        assert!(is_organization_capability_failure(
            "read knowledge answer: connection reset"
        ));
        assert!(is_organization_capability_failure(
            "knowledge ask: HTTP 503 Service Unavailable"
        ));
        assert!(!is_organization_capability_failure(
            "target_scope is required"
        ));
        assert!(!is_organization_capability_failure(
            "knowledge ask: HTTP 400 Bad Request"
        ));
    }

    #[test]
    fn late_authenticated_response_cannot_commit_to_a_new_account_generation() {
        let old_request = AccountContext {
            account_id: "account-a".into(),
            generation: 7,
        };
        assert!(account_context_matches(&old_request, 7, Some("account-a")));
        assert!(!account_context_matches(&old_request, 8, Some("account-a")));
        assert!(!account_context_matches(&old_request, 7, Some("account-b")));
        assert!(!account_context_matches(&old_request, 7, None));
    }

    #[test]
    fn late_model_response_cannot_overwrite_a_newer_sync() {
        assert!(require_current_model_epoch(8, 8).is_ok());
        assert!(require_current_model_epoch(7, 8).is_err());
    }

    #[test]
    fn local_store_reader_rejects_oversized_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.json");
        std::fs::write(&path, vec![b'x'; 33]).unwrap();
        assert!(read_file_bounded(&path, 32, "test store").is_err());
    }

    #[test]
    fn local_store_defaults_only_when_missing_and_rejects_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.json");
        let missing = read_json_or_default_if_missing::<SkillSyncState>(&path).unwrap();
        assert_eq!(missing, SkillSyncState::default());

        std::fs::write(&path, b"{ definitely-not-json }").unwrap();
        assert!(read_json_or_default_if_missing::<SkillSyncState>(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn local_store_reader_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.json");
        let link = temp.path().join("store.json");
        std::fs::write(&target, b"{}").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read_file_bounded(&link, 32, "test store").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn managed_skill_transaction_rejects_symlink_lock_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.lock");
        std::fs::write(&target, b"").unwrap();
        symlink(&target, temp.path().join(SKILL_STATE_LOCK_FILE)).unwrap();
        let inner = OrgInner {
            client: reqwest::Client::new(),
            session: AsyncMutex::new(OrgSession::default()),
            skill_sync: AsyncMutex::new(()),
            skill_state_transaction: Mutex::new(()),
            model_state_transaction: Mutex::new(()),
            skill_epoch: AtomicU64::new(0),
            model_epoch: AtomicU64::new(0),
            account_generation: AtomicU64::new(0),
        };
        let state_path = temp.path().join(SKILL_STATE_FILE);
        assert!(with_skill_state_transaction_at(&inner, &state_path, || Ok(())).is_err());
    }

    #[test]
    fn managed_skill_state_fields_are_bounded_before_crypto_or_io() {
        let id = Uuid::nil().to_string();
        let item = InstalledSkill {
            skill_id: id.clone(),
            version_id: Uuid::now_v7().to_string(),
            name: "x".repeat(MAX_PROFILE_FIELD_CHARS + 1),
            ..Default::default()
        };
        assert!(validate_installed_skill_fields(&id, &item).is_err());

        let mut invalid_state = SkillSyncState {
            account_id: "account".into(),
            revision: "not-a-uuid".into(),
            ..Default::default()
        };
        assert!(validate_skill_state(&invalid_state).is_err());
        invalid_state.revision = Uuid::now_v7().to_string();
        assert!(validate_skill_state(&invalid_state).is_ok());
    }

    #[test]
    fn managed_skill_commit_rejects_stale_epoch_account_and_disk_revision() {
        let base = SkillSyncState {
            account_id: "account-a".into(),
            revision: Uuid::now_v7().to_string(),
            cursor: "cursor-a".into(),
            ..Default::default()
        };
        assert!(require_skill_commit_current(7, 7, "account-a", "account-a", &base, &base).is_ok());
        assert!(
            require_skill_commit_current(7, 8, "account-a", "account-a", &base, &base).is_err()
        );
        assert!(
            require_skill_commit_current(7, 7, "account-a", "account-b", &base, &base).is_err()
        );
        let mut concurrent = base.clone();
        concurrent.revision = Uuid::now_v7().to_string();
        assert!(
            require_skill_commit_current(7, 7, "account-a", "account-a", &base, &concurrent)
                .is_err()
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_dpapi_round_trip_uses_current_user() {
        let plaintext = "EchoAgent 组织凭据 \0 🔐".as_bytes();
        let encrypted = dpapi_protect(plaintext).unwrap();

        assert_ne!(encrypted, plaintext);
        assert_eq!(dpapi_unprotect(&encrypted).unwrap(), plaintext);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_dpapi_rejects_corrupt_ciphertext() {
        let error = dpapi_unprotect(b"not-a-dpapi-blob").unwrap_err();

        assert!(error.starts_with("DPAPI decryption failed:"));
    }

    #[test]
    fn document_fetch_body_omits_missing_page() {
        assert_eq!(
            document_fetch_body("doc-1".into(), None),
            json!({ "docId": "doc-1" })
        );
        assert_eq!(
            document_fetch_body("doc-1".into(), Some(3)),
            json!({ "docId": "doc-1", "page": 3 })
        );
    }

    #[test]
    fn sse_parser_preserves_unicode_split_between_network_chunks() {
        let frame = "event: delta\ndata: {\"text\":\"组织\"}\n\n";
        let mut pending = Vec::new();
        let mut events = Vec::new();
        for chunk in frame.as_bytes().chunks(2) {
            pending.extend_from_slice(chunk);
            events.extend(drain_sse_events(&mut pending).unwrap());
        }
        assert!(pending.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "delta");
        assert_eq!(events[0].1, json!({ "text": "组织" }));
    }

    #[test]
    fn sse_parser_accepts_crlf_and_multiline_data() {
        let mut pending = b"event: status\r\ndata: first\r\ndata: second\r\n\r\n".to_vec();
        let events = drain_sse_events(&mut pending).unwrap();
        assert!(pending.is_empty());
        assert_eq!(
            events,
            vec![("status".into(), Value::String("first\nsecond".into()))]
        );
    }

    fn installed(id: &str, scope: &str, protected: bool) -> InstalledSkill {
        InstalledSkill {
            skill_id: id.into(),
            version_id: "v1".into(),
            name: "shared-skill".into(),
            path: format!("/{id}"),
            scope_kind: scope.into(),
            mandatory: false,
            allow_personal_override: !protected,
            ..Default::default()
        }
    }

    #[test]
    fn managed_skill_precedence_honors_scope_and_enterprise_lock() {
        let personal = installed("personal", "personal", false);
        let team = installed("team", "team", false);
        let locked_org = installed("org", "org", true);
        assert!(skill_precedes(&personal, &team));
        assert!(!skill_precedes(&team, &personal));
        assert!(skill_precedes(&locked_org, &personal));
    }

    #[test]
    fn authoritative_visibility_removes_skills_after_scope_revocation() {
        let mut state = SkillSyncState::default();
        state
            .installed
            .insert("visible".into(), installed("visible", "team", false));
        state
            .installed
            .insert("revoked".into(), installed("revoked", "team", false));
        let removed = reconcile_visible_skills(&mut state, &HashSet::from(["visible".to_string()]));
        assert!(state.installed.contains_key("visible"));
        assert!(!state.installed.contains_key("revoked"));
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].skill_id, "revoked");
    }

    #[test]
    fn extracted_managed_skill_must_exactly_match_signed_zip() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("SKILL.md", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(b"---\nname: verified\nversion: 1.0.0\n---\n")
            .unwrap();
        writer
            .start_file(
                "references/policy.md",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"approved policy").unwrap();
        writer
            .start_file(
                "references/example/SKILL.md",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"nested reference instructions").unwrap();
        let package = writer.finish().unwrap().into_inner();

        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("references/example")).unwrap();
        std::fs::write(
            temp.path().join("SKILL.md"),
            b"---\nname: verified\nversion: 1.0.0\n---\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("references/policy.md"), b"approved policy").unwrap();
        std::fs::write(
            temp.path().join("references/example/SKILL.md"),
            b"nested reference instructions",
        )
        .unwrap();
        assert!(verify_extracted_skill(&package, temp.path()).is_ok());

        std::fs::write(temp.path().join("SKILL.md"), b"tampered instructions").unwrap();
        assert!(verify_extracted_skill(&package, temp.path()).is_err());
        std::fs::write(
            temp.path().join("SKILL.md"),
            b"---\nname: verified\nversion: 1.0.0\n---\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("hidden.sh"), b"echo hidden").unwrap();
        assert!(verify_extracted_skill(&package, temp.path()).is_err());
    }
}
