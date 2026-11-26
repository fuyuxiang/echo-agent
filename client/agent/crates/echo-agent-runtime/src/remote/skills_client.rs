//! cloud.echo-agent.invalid product Skills catalog — the same REST sources echo-agent-web uses:
//! - `POST /rest/skills` — first-party bundled skills (docx, pdf, ffmpeg, …)
//! - `GET  /rest/user-skills` — enabled user-uploaded skills
//!
//! Transport only. Chat `echo.agent/commands/list` / ACP `available_commands_update`
//! map this catalog to slash commands.
//!
//! Desktop/shell chat uses this REST path (not gateway
//! `conversation.commands.updated`): one process-local source for ACU,
//! list_commands, and slash resolve/expansion. Gateway command updates are
//! the web product rail and are not bridged into ACP here.
//!
//! **Bodies / expansion.** List endpoints return names/descriptions (and
//! optional `skill_md_content` for user skills). Bundled rows are advertised
//! with `body: None` and a synthetic `chat-product://` path — shell does not
//! load a local SKILL.md for them. Chat turn expansion for those entries is
//! product/gateway-side; shell only expands when a body is preloaded
//! (user skills with `skill_md_content`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use echo_agent_tools::implementations::skills::skill::extract_skill_body;
use echo_agent_tools::implementations::skills::types::{SkillInfo, SkillScope};
use serde::Deserialize;

use crate::auth::AuthManager;

const ECHO_AGENT_WEB_URL: &str = "https://cloud.echo-agent.invalid";

/// Marker stored on SkillInfo.metadata / AvailableCommand._meta so clients
/// can tell product Skills from Build disk discovery without name allowlists.
pub const CHAT_PRODUCT_META_VALUE: &str = "chat";
pub const CHAT_PRODUCT_META_KEY: &str = "product";

const LIST_CATALOG_ATTEMPTS: u32 = 3;
const LIST_CATALOG_BACKOFF: Duration = Duration::from_millis(100);
/// Per-request budget for product Skills REST. Shared client only sets a
/// connect timeout; without this a hung cloud.echo-agent.invalid stalls the session actor.
const LIST_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundledSkill {
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListBundledSkillsResponse {
    #[serde(default)]
    pub skills: Vec<BundledSkill>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSkill {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Defaults to true when omitted (proto3 optional).
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub skill_md_content: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListUserSkillsResponse {
    #[serde(default)]
    pub skills: Vec<UserSkill>,
}

/// Combined product catalog matching echo-agent-web's Skills menu composition.
#[derive(Debug, Clone, Default)]
pub struct ProductSkillsCatalog {
    pub bundled: Vec<BundledSkill>,
    pub user: Vec<UserSkill>,
    /// True when `/rest/user-skills` failed after retries. Empty `user` is then
    /// *not* an authoritative empty list — callers must not overwrite a prior
    /// full catalog with this degraded result.
    pub user_list_failed: bool,
}

impl ProductSkillsCatalog {
    /// Map to agent SkillInfo rows for slash advertising.
    ///
    /// Mirrors echo-agent-web: enabled user skills first (and hide same-named
    /// bundled entries they override), then remaining bundled skills.
    pub(crate) fn to_skill_infos(&self) -> Vec<SkillInfo> {
        let enabled_user: Vec<(String, &UserSkill)> = self
            .user
            .iter()
            .filter_map(|s| {
                if !s.enabled {
                    return None;
                }
                let name = s.name.trim();
                if name.is_empty() {
                    return None;
                }
                Some((name.to_string(), s))
            })
            .collect();

        let mut out = Vec::with_capacity(enabled_user.len() + self.bundled.len());
        // Override/dedupe keys are slash command names (slugified), not raw
        // display titles — user "Docx" must hide bundled "docx".
        let mut user_command_names = std::collections::HashSet::new();

        for (name, skill) in &enabled_user {
            let command_name = slugify_skill_command_name(name);
            if !user_command_names.insert(command_name.clone()) {
                continue;
            }
            let body = skill.skill_md_content.as_ref().and_then(|raw| {
                let stripped = extract_skill_body(raw);
                if stripped.is_empty() {
                    None
                } else {
                    Some(stripped)
                }
            });
            let display_name = if command_name == *name {
                None
            } else {
                Some(name.clone())
            };
            out.push(product_skill_info(
                &command_name,
                skill.description.clone(),
                display_name,
                SkillScope::User,
                None,
                body,
            ));
        }

        let mut bundled: Vec<&BundledSkill> = self.bundled.iter().collect();
        bundled.sort_by_key(|s| (s.index, s.name.as_str()));
        for skill in bundled {
            let name = skill.name.trim();
            if name.is_empty() {
                continue;
            }
            let command_name = slugify_skill_command_name(name);
            if user_command_names.contains(&command_name) {
                continue;
            }
            let display = skill.display_name.trim();
            let display_name = if !display.is_empty() && display != command_name {
                Some(display.to_string())
            } else if command_name != name {
                Some(name.to_string())
            } else {
                None
            };
            let icon = skill.icon.trim();
            out.push(product_skill_info(
                &command_name,
                skill.description.clone(),
                display_name,
                SkillScope::Server,
                if icon.is_empty() {
                    None
                } else {
                    Some(icon.to_string())
                },
                None,
            ));
        }

        out
    }
}

/// Slash-safe skill command name: lowercase alphanumerics (unicode kept),
/// non-alnum runs → `-`, trim `-`. Empty results get a stable `skill-<hash>`
/// fallback so non-ASCII-only titles still advertise.
fn slugify_skill_command_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut hasher);
        format!("skill-{:x}", hasher.finish())
    } else {
        out
    }
}

fn product_skill_info(
    name: &str,
    description: String,
    display_name: Option<String>,
    scope: SkillScope,
    icon: Option<String>,
    body: Option<String>,
) -> SkillInfo {
    let mut metadata = HashMap::new();
    metadata.insert(
        CHAT_PRODUCT_META_KEY.to_string(),
        CHAT_PRODUCT_META_VALUE.to_string(),
    );
    if let Some(icon) = icon {
        metadata.insert("icon".to_string(), icon);
    }

    SkillInfo {
        name: name.to_string(),
        display_name,
        description,
        has_user_specified_description: true,
        paths: None,
        when_to_use: None,
        short_description: None,
        author: None,
        argument_hint: None,
        license: None,
        compatibility: None,
        metadata: Some(metadata),
        // Synthetic path — product skills are not disk SKILL.md discovery.
        // Prefer in-memory `body` (user skill_md_content) when present.
        path: format!("chat-product://{name}"),
        scope,
        config_source: None,
        plugin_name: None,
        plugin_version: None,
        plugin_root: None,
        plugin_data: None,
        allowed_tools: None,
        model: None,
        effort: None,
        user_invocable: true,
        disable_model_invocation: false,
        enabled: true,
        body,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SkillsError {
    #[error("no cloud.echo-agent.invalid credentials")]
    NoAuth,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("request failed: {status}")]
    Http { status: u16 },
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

impl SkillsError {
    fn is_retryable(&self) -> bool {
        match self {
            SkillsError::Network(_) => true,
            SkillsError::Http { status } => *status >= 500,
            SkillsError::NoAuth | SkillsError::Parse(_) => false,
        }
    }
}

/// One credential to try for product Skills REST.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillsAuthCandidate {
    key: String,
    user_id: String,
    email: Option<String>,
    /// Untagged same-user alt used when primary is tenant-tagged (OIDC 403
    /// recovery). Catalog is still success-cached under the **primary**
    /// team/org identity so the same team session can hit TTL.
    untagged_recovery: bool,
}

fn primary_is_tenant_tagged(auth: &crate::auth::EchoAgentAuth) -> bool {
    auth.team_id.is_some() || auth.organization_id.is_some()
}

fn entry_is_untagged(auth: &crate::auth::EchoAgentAuth) -> bool {
    auth.team_id.is_none() && auth.organization_id.is_none()
}

/// Exact tenant equality for tagged credentials.
///
/// When either side carries team and/or org, both `team_id` and
/// `organization_id` must match (including both `None`). Prevents accepting
/// an alt that is a strict superset of primary tags (e.g. primary team-only
/// matching team+org).
fn entry_matches_primary_tenant(
    primary: &crate::auth::EchoAgentAuth,
    entry: &crate::auth::EchoAgentAuth,
) -> bool {
    if !primary_is_tenant_tagged(primary) && !primary_is_tenant_tagged(entry) {
        return false;
    }
    primary.team_id == entry.team_id && primary.organization_id == entry.organization_id
}

/// Build ordered alt credentials for product Skills REST (after primary).
///
/// - Prefer same-tagged alts when primary is tagged (exact team+org equality).
/// - Allow untagged same-user alts as OIDC/team 403 recovery when primary is
///   tagged (catalog still cached under primary identity).
/// - Untagged primary never accepts more-tagged (team/org) alts.
fn skills_auth_alt_candidates<'a>(
    primary: &crate::auth::EchoAgentAuth,
    entries: impl IntoIterator<Item = &'a crate::auth::EchoAgentAuth>,
) -> Vec<SkillsAuthCandidate> {
    let mut tagged_match = Vec::new();
    let mut untagged = Vec::new();
    let primary_tagged = primary_is_tenant_tagged(primary);

    for entry in entries {
        if entry_matches_primary_tenant(primary, entry) {
            tagged_match.push(SkillsAuthCandidate {
                key: entry.key.clone(),
                user_id: entry.user_id.clone(),
                email: entry.email.clone(),
                untagged_recovery: false,
            });
            continue;
        }
        if entry_is_untagged(entry) {
            untagged.push(SkillsAuthCandidate {
                key: entry.key.clone(),
                user_id: entry.user_id.clone(),
                email: entry.email.clone(),
                // Recovery only when primary is tenant-scoped; for personal
                // primary this is a normal same-scope alt.
                untagged_recovery: primary_tagged,
            });
        }
        // Drop: tagged alt that does not match primary (includes team-tagged
        // alt when primary is personal, or extra org/team tags).
    }

    let mut out = tagged_match;
    out.extend(untagged);
    out
}

/// Stateless transport for product Skills REST (echo-agent-web `skillsApi`).
pub struct SkillsClient {
    http: reqwest::Client,
    base_url: String,
    auth: Arc<AuthManager>,
}

impl SkillsClient {
    pub fn new(auth: Arc<AuthManager>) -> Self {
        let base_url = std::env::var("ECHO_AGENT_SKILLS_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("ECHO_AGENT_CONVERSATIONS_BASE_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                std::env::var("ECHO_AGENT_CODE_WEB_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| ECHO_AGENT_WEB_URL.to_string());
        Self {
            http: crate::http::shared_client(),
            base_url,
            auth,
        }
    }

    fn apply_auth_headers(
        &self,
        builder: reqwest::RequestBuilder,
        key: &str,
        user_id: &str,
        email: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut builder = builder
            .header("Authorization", format!("Bearer {key}"))
            .header(
                "X-Echo-Agent-Token-Auth",
                self.auth.echo_agent_com_config().token_header.clone(),
            )
            .header("x-userid", user_id)
            .header("x-echo-agent-client-version", echo_agent_version::VERSION)
            .header(
                "x-echo-agent-client-identifier",
                crate::http::process_client_identifier(),
            )
            .header(
                crate::http::CLIENT_MODE_HEADER,
                crate::http::process_client_mode(),
            )
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(email) = email {
            builder = builder.header("x-email", email);
        }
        echo_agent_file_utils::trace_context::inject_trace_context_into_request(builder)
    }

    /// EchoAgent hosted Skills require first-party session auth (same gate as
    /// managed MCP / sibling cloud.echo-agent.invalid clients — not plain BYOK API keys).
    async fn require_skills_auth(&self) -> Result<crate::auth::EchoAgentAuth, SkillsError> {
        let auth = self.auth.auth().await.map_err(|_| SkillsError::NoAuth)?;
        if !auth.is_managed_mcp_eligible() {
            return Err(SkillsError::NoAuth);
        }
        Ok(auth)
    }

    /// Credentials to try for cloud.echo-agent.invalid product Skills REST.
    ///
    /// Primary first. When primary is OIDC on the default cloud.echo-agent.invalid host, also
    /// try non-OIDC keys for the same user from this AuthManager's `auth.json`
    /// (team OIDC is often rejected with `oauth2-auth-forbidden`).
    ///
    /// Order / isolation (see [`skills_auth_alt_candidates`]):
    /// 1. same-tenant-tagged alts first when primary is tagged
    /// 2. untagged same-user alts as 403 recovery when primary is tagged
    /// 3. untagged primary never accepts team-tagged alts
    fn skills_auth_candidates(
        &self,
        primary: &crate::auth::EchoAgentAuth,
    ) -> Vec<SkillsAuthCandidate> {
        use crate::auth::AuthMode;
        let mut out = vec![SkillsAuthCandidate {
            key: primary.key.clone(),
            user_id: primary.user_id.clone(),
            email: primary.email.clone(),
            untagged_recovery: false,
        }];
        if primary.auth_mode != AuthMode::Oidc || primary.user_id.is_empty() {
            return out;
        }
        if self.base_url != ECHO_AGENT_WEB_URL {
            return out;
        }
        let Ok(store) = crate::auth::read_auth_json(self.auth.auth_json_path()) else {
            return out;
        };
        out.extend(skills_auth_alt_candidates(
            primary,
            store.values().filter(|e| {
                e.auth_mode != AuthMode::Oidc
                    && !e.key.is_empty()
                    && e.key != primary.key
                    && e.user_id == primary.user_id
            }),
        ));
        out
    }

    async fn list_bundled_with_key(
        &self,
        locale: &str,
        key: &str,
        user_id: &str,
        email: Option<&str>,
    ) -> Result<ListBundledSkillsResponse, SkillsError> {
        let url = format!("{}/rest/skills", self.base_url);
        let body = serde_json::json!({ "locale": locale });
        let builder = self
            .apply_auth_headers(self.http.post(&url).json(&body), key, user_id, email)
            .timeout(LIST_REQUEST_TIMEOUT);
        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(SkillsError::Http {
                status: status.as_u16(),
            });
        }
        let bytes = response.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn list_user_with_key(
        &self,
        key: &str,
        user_id: &str,
        email: Option<&str>,
    ) -> Result<ListUserSkillsResponse, SkillsError> {
        let url = format!("{}/rest/user-skills", self.base_url);
        let builder = self
            .apply_auth_headers(self.http.get(&url), key, user_id, email)
            .timeout(LIST_REQUEST_TIMEOUT);
        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(SkillsError::Http {
                status: status.as_u16(),
            });
        }
        let bytes = response.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Bundled first-party skills (`POST /rest/skills`), gated by backend
    /// feature-flag allow-list (same as echo-agent-web attach menu).
    ///
    /// Returns `(response, used_untagged_recovery)`.
    async fn list_bundled(
        &self,
        locale: &str,
    ) -> Result<(ListBundledSkillsResponse, bool), SkillsError> {
        let auth = self.require_skills_auth().await?;
        let candidates = self.skills_auth_candidates(&auth);
        let mut last_err = SkillsError::NoAuth;
        for (i, cand) in candidates.iter().enumerate() {
            match self
                .list_bundled_with_key(locale, &cand.key, &cand.user_id, cand.email.as_deref())
                .await
            {
                Ok(r) => {
                    if i > 0 {
                        tracing::info!(
                            untagged_recovery = cand.untagged_recovery,
                            "product skills: bundled list succeeded with non-primary credentials"
                        );
                    }
                    return Ok((r, cand.untagged_recovery));
                }
                Err(err) => {
                    if matches!(err, SkillsError::Http { status: 403 }) && i + 1 < candidates.len()
                    {
                        tracing::warn!(
                            error = %err,
                            "product skills: bundled list 403 — trying alternate credentials"
                        );
                        last_err = err;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err(last_err)
    }

    /// User custom skills (`GET /rest/user-skills`).
    ///
    /// Returns `(response, used_untagged_recovery)`.
    async fn list_user(&self) -> Result<(ListUserSkillsResponse, bool), SkillsError> {
        let auth = self.require_skills_auth().await?;
        let candidates = self.skills_auth_candidates(&auth);
        let mut last_err = SkillsError::NoAuth;
        for (i, cand) in candidates.iter().enumerate() {
            match self
                .list_user_with_key(&cand.key, &cand.user_id, cand.email.as_deref())
                .await
            {
                Ok(r) => {
                    if i > 0 {
                        tracing::info!(
                            untagged_recovery = cand.untagged_recovery,
                            "product skills: user list succeeded with non-primary credentials"
                        );
                    }
                    return Ok((r, cand.untagged_recovery));
                }
                Err(err) => {
                    if matches!(err, SkillsError::Http { status: 403 }) && i + 1 < candidates.len()
                    {
                        tracing::warn!(
                            error = %err,
                            "product skills: user list 403 — trying alternate credentials"
                        );
                        last_err = err;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err(last_err)
    }

    async fn list_bundled_with_retry(
        &self,
        locale: &str,
    ) -> Result<(Vec<BundledSkill>, bool), SkillsError> {
        let mut last_err = None;
        for attempt in 1..=LIST_CATALOG_ATTEMPTS {
            match self.list_bundled(locale).await {
                // Empty 200 is authoritative — do not substitute a catalog.
                Ok((r, recovery)) => return Ok((r.skills, recovery)),
                Err(err) if attempt < LIST_CATALOG_ATTEMPTS && err.is_retryable() => {
                    tracing::warn!(
                        error = %err,
                        attempt,
                        "product skills: bundled list failed — retrying"
                    );
                    tokio::time::sleep(LIST_CATALOG_BACKOFF * attempt).await;
                    last_err = Some(err);
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_err.unwrap_or(SkillsError::NoAuth))
    }

    async fn list_user_with_retry(&self) -> Result<(Vec<UserSkill>, bool), SkillsError> {
        let mut last_err = None;
        for attempt in 1..=LIST_CATALOG_ATTEMPTS {
            match self.list_user().await {
                Ok((r, recovery)) => return Ok((r.skills, recovery)),
                Err(err) if attempt < LIST_CATALOG_ATTEMPTS && err.is_retryable() => {
                    tracing::warn!(
                        error = %err,
                        attempt,
                        "product skills: user list failed — retrying"
                    );
                    tokio::time::sleep(LIST_CATALOG_BACKOFF * attempt).await;
                    last_err = Some(err);
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_err.unwrap_or(SkillsError::NoAuth))
    }

    /// Full product catalog (bundled + user).
    ///
    /// Empty REST 200 is authoritative (no embedded substitute). Transient
    /// transport / 5xx failures retry a few times. Bundled REST failure after
    /// retries is `Err` (callers must not invent a catalog). User REST failure
    /// yields empty user skills with `user_list_failed: true` while keeping
    /// bundled — callers must not treat that as an authoritative empty user
    /// list (e.g. must not poison a last-success cache).
    ///
    /// The bool is `used_untagged_recovery`: catalog loaded via an untagged
    /// alt while primary is tenant-tagged. Callers still success-cache under
    /// the **primary** identity (team/org of primary) so the same session
    /// hits TTL; personal primaries cannot match that entry.
    pub(crate) async fn try_list_catalog(
        &self,
        locale: &str,
    ) -> Result<(ProductSkillsCatalog, bool), SkillsError> {
        let (bundled_res, user_res) = tokio::join!(
            self.list_bundled_with_retry(locale),
            self.list_user_with_retry(),
        );
        let (bundled, bundled_recovery) = bundled_res.map_err(|err| {
            tracing::warn!(
                error = %err,
                "product skills: bundled list failed after retries"
            );
            err
        })?;
        let (user, user_list_failed, user_recovery) = match user_res {
            Ok((skills, recovery)) => (skills, false, recovery),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "product skills: user list failed after retries — empty user skills"
                );
                (Vec::new(), true, false)
            }
        };
        Ok((
            ProductSkillsCatalog {
                bundled,
                user,
                user_list_failed,
            },
            bundled_recovery || user_recovery,
        ))
    }

    /// Like [`Self::try_list_catalog`], but maps bundled failure to an empty
    /// catalog (still no embedded fallback names). Prefer `try_list_catalog`
    /// when callers must distinguish empty-success from failure.
    pub async fn list_catalog(&self, locale: &str) -> ProductSkillsCatalog {
        self.try_list_catalog(locale)
            .await
            .map(|(catalog, _)| catalog)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn with_base_url(auth: Arc<AuthManager>, base_url: impl Into<String>) -> Self {
        Self {
            http: crate::http::shared_client(),
            base_url: base_url.into(),
            auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_parse_camelcase_wire() {
        let json = serde_json::json!({
            "skills": [{
                "index": 1,
                "name": "docx",
                "description": "Word docs",
                "icon": "file-text",
                "displayName": "Word Documents"
            }, {
                "name": "ffmpeg",
                "description": "Media"
            }]
        });
        let resp: ListBundledSkillsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.skills.len(), 2);
        assert_eq!(resp.skills[0].name, "docx");
        assert_eq!(resp.skills[0].display_name, "Word Documents");
        assert_eq!(resp.skills[0].icon, "file-text");
        assert_eq!(resp.skills[1].name, "ffmpeg");
    }

    #[test]
    fn user_parse_and_catalog_composition() {
        let json = serde_json::json!({
            "skills": [{
                "name": "review",
                "description": "My review skill",
                "enabled": true,
                "id": "abc"
            }, {
                "name": "disabled-one",
                "enabled": false
            }, {
                "name": "docx",
                "description": "User override of docx",
                "enabled": true
            }]
        });
        let user: ListUserSkillsResponse = serde_json::from_value(json).unwrap();
        let catalog = ProductSkillsCatalog {
            bundled: vec![
                BundledSkill {
                    name: "docx".into(),
                    description: "Bundled docx".into(),
                    display_name: "Word".into(),
                    icon: "file-text".into(),
                    ..Default::default()
                },
                BundledSkill {
                    name: "pdf".into(),
                    description: "PDFs".into(),
                    ..Default::default()
                },
            ],
            user: user.skills,
            user_list_failed: false,
        };
        let infos = catalog.to_skill_infos();
        let names: Vec<_> = infos.iter().map(|s| s.name.as_str()).collect();
        // user review + user docx override; bundled pdf; no disabled; no bundled docx
        assert_eq!(names, vec!["review", "docx", "pdf"]);
        assert_eq!(infos[0].scope, SkillScope::User);
        assert_eq!(infos[1].scope, SkillScope::User);
        assert_eq!(infos[2].scope, SkillScope::Server);
        assert_eq!(
            infos[0].metadata.as_ref().unwrap().get("product").unwrap(),
            "chat"
        );
        assert!(infos[0].path.starts_with("chat-product://"));
    }

    #[test]
    fn user_enabled_defaults_true_when_omitted() {
        let json = serde_json::json!({ "skills": [{ "name": "review" }] });
        let resp: ListUserSkillsResponse = serde_json::from_value(json).unwrap();
        assert!(resp.skills[0].enabled);
    }

    #[test]
    fn empty_rest_catalog_stays_empty() {
        let infos = ProductSkillsCatalog {
            bundled: vec![],
            user: vec![],
            user_list_failed: false,
        }
        .to_skill_infos();
        assert!(infos.is_empty());
    }

    #[test]
    fn retryable_transport_and_5xx_only() {
        assert!(SkillsError::Http { status: 503 }.is_retryable());
        assert!(SkillsError::Http { status: 500 }.is_retryable());
        assert!(!SkillsError::Http { status: 403 }.is_retryable());
        assert!(!SkillsError::Http { status: 404 }.is_retryable());
        assert!(!SkillsError::NoAuth.is_retryable());
    }

    fn test_auth_manager() -> Arc<AuthManager> {
        use crate::auth::{AuthMode, ECHO_AGENT_OAUTH2_ISSUER, EchoAgentAuth, EchoAgentComConfig};
        let dir = tempfile::tempdir().unwrap();
        let mgr = AuthManager::new(dir.path(), EchoAgentComConfig::default());
        mgr.hot_swap(EchoAgentAuth {
            key: "token".into(),
            auth_mode: AuthMode::Oidc,
            create_time: chrono::Utc::now(),
            user_id: "user-1".into(),
            email: Some("test@example.com".into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
            ..Default::default()
        });
        std::mem::forget(dir);
        Arc::new(mgr)
    }

    async fn spawn_skills_mock(
        bundled_status: u16,
        bundled_body: &'static str,
        user_status: u16,
        user_body: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use axum::Router;
        use axum::body::Body;
        use axum::http::{Response, StatusCode};
        use axum::routing::{get, post};
        use std::sync::atomic::{AtomicU32, Ordering};

        let bundled_hits = Arc::new(AtomicU32::new(0));
        let user_hits = Arc::new(AtomicU32::new(0));
        let bundled_hits_post = bundled_hits.clone();
        let user_hits_get = user_hits.clone();

        let app = Router::new()
            .route(
                "/rest/skills",
                post(move || {
                    let hits = bundled_hits_post.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Response::builder()
                            .status(StatusCode::from_u16(bundled_status).unwrap())
                            .header("content-type", "application/json")
                            .body(Body::from(bundled_body))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/rest/user-skills",
                get(move || {
                    let hits = user_hits_get.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Response::builder()
                            .status(StatusCode::from_u16(user_status).unwrap())
                            .header("content-type", "application/json")
                            .body(Body::from(user_body))
                            .unwrap()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_rest_200_is_authoritative() {
        let (base, handle) =
            spawn_skills_mock(200, r#"{"skills":[]}"#, 200, r#"{"skills":[]}"#).await;
        let client = SkillsClient::with_base_url(test_auth_manager(), base);
        let (catalog, recovery) = client.try_list_catalog("en").await.expect("ok empty");
        assert!(catalog.bundled.is_empty());
        assert!(catalog.user.is_empty());
        assert!(catalog.to_skill_infos().is_empty());
        assert!(!recovery);
        handle.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rest_error_after_retries_is_err_not_fallback_names() {
        let (base, handle) =
            spawn_skills_mock(503, r#"{"error":"unavailable"}"#, 200, r#"{"skills":[]}"#).await;
        let client = SkillsClient::with_base_url(test_auth_manager(), base);
        let err = client
            .try_list_catalog("en")
            .await
            .expect_err("bundled fail");
        assert!(matches!(err, SkillsError::Http { status: 503 }));
        // list_catalog collapses failure to empty — still no docx/pdf/etc.
        let collapsed = client.list_catalog("en").await;
        let names: Vec<_> = collapsed
            .to_skill_infos()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(
            names.is_empty(),
            "must not inject embedded fallback skills, got {names:?}"
        );
        handle.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn success_bundled_skills_pass_through() {
        let body = r#"{"skills":[{"name":"docx","description":"Word","displayName":"Word Documents","icon":"file-text"}]}"#;
        let (base, handle) = spawn_skills_mock(200, body, 200, r#"{"skills":[]}"#).await;
        let client = SkillsClient::with_base_url(test_auth_manager(), base);
        let (catalog, _) = client.try_list_catalog("en").await.unwrap();
        let infos = catalog.to_skill_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "docx");
        assert!(infos[0].path.starts_with("chat-product://"));
        handle.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn user_list_failure_sets_flag_keeps_bundled() {
        let bundled = r#"{"skills":[{"name":"docx","description":"Word"}]}"#;
        let (base, handle) =
            spawn_skills_mock(200, bundled, 503, r#"{"error":"unavailable"}"#).await;
        let client = SkillsClient::with_base_url(test_auth_manager(), base);
        let (catalog, _) = client.try_list_catalog("en").await.expect("bundled ok");
        assert!(catalog.user_list_failed);
        assert!(catalog.user.is_empty());
        assert_eq!(catalog.bundled.len(), 1);
        handle.abort();
    }

    #[test]
    fn skills_auth_alts_prefer_tagged_then_untagged_recovery() {
        use crate::auth::{AuthMode, EchoAgentAuth};
        let primary = EchoAgentAuth {
            key: "oidc".into(),
            user_id: "u1".into(),
            team_id: Some("team-a".into()),
            organization_id: None,
            auth_mode: AuthMode::Oidc,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let same_team = EchoAgentAuth {
            key: "web-team".into(),
            user_id: "u1".into(),
            team_id: Some("team-a".into()),
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let untagged = EchoAgentAuth {
            key: "web-personal".into(),
            user_id: "u1".into(),
            team_id: None,
            organization_id: None,
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let other_team = EchoAgentAuth {
            key: "web-other".into(),
            user_id: "u1".into(),
            team_id: Some("team-b".into()),
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        // Extra org tag must not match team-only primary (symmetric equality).
        let team_plus_org = EchoAgentAuth {
            key: "web-team-org".into(),
            user_id: "u1".into(),
            team_id: Some("team-a".into()),
            organization_id: Some("org-x".into()),
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let alts = skills_auth_alt_candidates(
            &primary,
            [&same_team, &untagged, &other_team, &team_plus_org],
        );
        assert_eq!(alts.len(), 2);
        assert_eq!(alts[0].key, "web-team");
        assert!(!alts[0].untagged_recovery);
        assert_eq!(alts[1].key, "web-personal");
        assert!(alts[1].untagged_recovery);
    }

    #[test]
    fn skills_auth_alts_untagged_primary_rejects_team_keys() {
        use crate::auth::{AuthMode, EchoAgentAuth};
        let primary = EchoAgentAuth {
            key: "oidc".into(),
            user_id: "u1".into(),
            team_id: None,
            organization_id: None,
            auth_mode: AuthMode::Oidc,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let untagged = EchoAgentAuth {
            key: "web".into(),
            user_id: "u1".into(),
            team_id: None,
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let team = EchoAgentAuth {
            key: "web-team".into(),
            user_id: "u1".into(),
            team_id: Some("team-a".into()),
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let alts = skills_auth_alt_candidates(&primary, [&untagged, &team]);
        assert_eq!(alts.len(), 1);
        assert_eq!(alts[0].key, "web");
        assert!(!alts[0].untagged_recovery);
    }

    #[test]
    fn entry_matches_primary_tenant_requires_symmetric_tags() {
        use crate::auth::{AuthMode, EchoAgentAuth};
        let primary = EchoAgentAuth {
            key: "oidc".into(),
            user_id: "u1".into(),
            team_id: Some("team-a".into()),
            organization_id: None,
            auth_mode: AuthMode::Oidc,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        let same = EchoAgentAuth {
            key: "web".into(),
            user_id: "u1".into(),
            team_id: Some("team-a".into()),
            organization_id: None,
            auth_mode: AuthMode::WebLogin,
            create_time: chrono::Utc::now(),
            ..Default::default()
        };
        assert!(entry_matches_primary_tenant(&primary, &same));
        let extra_org = EchoAgentAuth {
            organization_id: Some("org".into()),
            ..same.clone()
        };
        assert!(!entry_matches_primary_tenant(&primary, &extra_org));
        let untagged = EchoAgentAuth {
            team_id: None,
            organization_id: None,
            ..same.clone()
        };
        assert!(!entry_matches_primary_tenant(&primary, &untagged));
    }

    #[test]
    fn skill_md_content_frontmatter_stripped_from_body() {
        let catalog = ProductSkillsCatalog {
            bundled: vec![],
            user: vec![UserSkill {
                name: "review".into(),
                description: "Review skill".into(),
                enabled: true,
                id: "1".into(),
                skill_md_content: Some(
                    "---\nname: review\ndescription: Review skill\n---\nDo the review.".into(),
                ),
            }],
            user_list_failed: false,
        };
        let infos = catalog.to_skill_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].body.as_deref(), Some("Do the review."));
    }

    #[test]
    fn user_skill_names_are_slugified_for_slash_commands() {
        let catalog = ProductSkillsCatalog {
            bundled: vec![],
            user: vec![UserSkill {
                name: "My Cool Review".into(),
                description: "Reviews things".into(),
                enabled: true,
                id: "1".into(),
                skill_md_content: None,
            }],
            user_list_failed: false,
        };
        let infos = catalog.to_skill_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "my-cool-review");
        assert_eq!(infos[0].display_name.as_deref(), Some("My Cool Review"));
    }

    #[test]
    fn bundled_skills_sorted_by_index() {
        let catalog = ProductSkillsCatalog {
            bundled: vec![
                BundledSkill {
                    index: 2,
                    name: "pdf".into(),
                    ..Default::default()
                },
                BundledSkill {
                    index: 1,
                    name: "docx".into(),
                    ..Default::default()
                },
            ],
            user: vec![],
            user_list_failed: false,
        };
        let names: Vec<_> = catalog
            .to_skill_infos()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["docx", "pdf"]);
    }

    #[test]
    fn slugify_skill_command_name_basic() {
        assert_eq!(slugify_skill_command_name("docx"), "docx");
        assert_eq!(slugify_skill_command_name("My Skill"), "my-skill");
        assert_eq!(slugify_skill_command_name("  spaced  "), "spaced");
        let fallback = slugify_skill_command_name("!!!");
        assert!(fallback.starts_with("skill-"), "{fallback}");
        assert_eq!(slugify_skill_command_name("!!!"), fallback);
        let unicode = slugify_skill_command_name("文档技能");
        assert!(!unicode.is_empty());
        assert!(!unicode.starts_with("skill-"), "{unicode}");
    }

    #[test]
    fn bundled_slug_keeps_raw_name_as_display_name() {
        let catalog = ProductSkillsCatalog {
            bundled: vec![BundledSkill {
                index: 1,
                name: "My Docx".into(),
                display_name: String::new(),
                description: "Bundled".into(),
                ..Default::default()
            }],
            user: vec![],
            user_list_failed: false,
        };
        let infos = catalog.to_skill_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "my-docx");
        assert_eq!(infos[0].display_name.as_deref(), Some("My Docx"));
    }

    #[test]
    fn user_slug_overrides_bundled_by_command_name() {
        let catalog = ProductSkillsCatalog {
            bundled: vec![BundledSkill {
                index: 1,
                name: "docx".into(),
                description: "Bundled docx".into(),
                ..Default::default()
            }],
            user: vec![UserSkill {
                name: "Docx".into(),
                description: "User docx override".into(),
                enabled: true,
                id: "u1".into(),
                skill_md_content: Some("user body".into()),
            }],
            user_list_failed: false,
        };
        let infos = catalog.to_skill_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "docx");
        assert_eq!(infos[0].scope, SkillScope::User);
        assert_eq!(infos[0].display_name.as_deref(), Some("Docx"));
    }

    #[test]
    fn user_slug_collisions_keep_first() {
        let catalog = ProductSkillsCatalog {
            bundled: vec![],
            user: vec![
                UserSkill {
                    name: "My Skill".into(),
                    description: "first".into(),
                    enabled: true,
                    id: "1".into(),
                    skill_md_content: None,
                },
                UserSkill {
                    name: "my-skill".into(),
                    description: "second".into(),
                    enabled: true,
                    id: "2".into(),
                    skill_md_content: None,
                },
            ],
            user_list_failed: false,
        };
        let infos = catalog.to_skill_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "my-skill");
        assert_eq!(infos[0].description, "first");
    }
}
