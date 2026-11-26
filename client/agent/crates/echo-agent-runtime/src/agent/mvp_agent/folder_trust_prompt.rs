//! Interactive folder-trust prompt: a dormant agent→GUI-client ACP round-trip
//! (`echo.agent/folder_trust/request`) that asks a GUI client (echo-agent-desktop) to decide
//! trust for an untrusted-with-configs workspace, then grants + reloads the
//! now-trusted project servers without a restart.
//!
//! DORMANT in production: it only fires when the connected client advertised
//! `echo.agent/folderTrust.interactive` AND the folder-trust feature flag is on AND the
//! verdict is [`echo_agent_workspace::folder_trust::TrustOutcome::Prompt`]. No
//! client advertises the capability until the desktop UI ships — so this is
//! inert by default even with the feature flag on. The TUI/headless clients never
//! advertise it (they self-gate trust client-side), so they are never
//! double-prompted. Co-located child of `mvp_agent` (`use super::*`).
//!
//! Post-grant reload scope: MCP, plugins, and each session's own project hooks
//! are hot-reloaded in place — for EVERY session sharing the granted workspace
//! (same `workspace_key`), each reloaded against its OWN cwd. Project LSP is NOT
//! hot-reloaded — the LSP backend is baked into the agent's tool bridge at build
//! time (one-shot startup coordinator, no in-place reconfigure API), so repo-local
//! `.echo-agent/lsp.json` servers start on the NEXT session open (the durable grant
//! makes the re-spawn trusted). `lsp` is still REPORTED in the prompt's
//! `configKinds` (it is a real reason the folder is gated) — only the post-grant
//! hot-reload skips it.

use super::*;

/// Max wait for a GUI client's trust decision before giving up (fail-closed).
/// Generous because it is a human decision, but bounds the detached task so a
/// connected-but-silent client (modal left open / client bug) can't leak it for
/// the whole connection lifetime.
const TRUST_PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// ACP `echo.agent/folder_trust/request` payload (agent → GUI client). Serialized as
/// `camelCase` for the ACP JSON-RPC wire format.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderTrustRequest {
    /// The session this prompt belongs to. REQUIRED for leader Tier-2 routing:
    /// non-interaction reverse-requests are delivered to the driver keyed on
    /// `params.sessionId`; omitting it makes the leader silently drop the message,
    /// so the prompt would never reach the client.
    pub session_id: String,
    /// The session cwd whose workspace is being gated.
    pub cwd: String,
    /// Display path of the canonical workspace key (the trust grant's scope).
    pub workspace: String,
    /// Detected repo-local config kinds (e.g. `mcp`, `hooks`, `lsp`) — the
    /// reasons the folder is gated — for the prompt UI. Display-only, NOT the
    /// trust gate; derived from the same scan as the gate. `lsp` may appear: it
    /// is a real reason to prompt, but project LSP applies on the NEXT session
    /// open rather than hot-reloading on grant (see module docs).
    pub config_kinds: Vec<String>,
}

/// Outcome of the trust prompt (GUI client → agent). Fail-closed: any value
/// other than `"trust"` (including unknown strings, via `#[serde(other)]`)
/// decodes to [`FolderTrustOutcome::Reject`], so only an explicit grant unblocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FolderTrustOutcome {
    Trust,
    #[serde(other)]
    Reject,
}

/// ACP `echo.agent/folder_trust/request` response (GUI client → agent).
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct FolderTrustResponse {
    pub outcome: FolderTrustOutcome,
}

impl MvpAgent {
    /// Parse the `echo.agent/folderTrust.interactive` capability from an initialize
    /// request. Returns `false` if absent or not `true`. Mirrors
    /// [`Self::parse_code_nav_capability`].
    pub(crate) fn parse_interactive_trust_capability(init: &acp::InitializeRequest) -> bool {
        init.client_capabilities
            .meta
            .as_ref()
            .and_then(|m| m.get("echo.agent/folderTrust"))
            .and_then(|v| v.get("interactive"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Ask a GUI client to decide trust for `session_id`'s workspace, then grant
    /// + reload on accept. DORMANT no-op unless the client advertised
    /// `echo.agent/folderTrust.interactive` AND [`folder_trust::prompt_warranted`]
    /// (feature on + untrusted + repo configs present).
    ///
    /// Non-blocking: the session was already created with project servers GATED
    /// (the untrusted resolve in `new_session`/`load_session`), so nothing
    /// repo-local spawns while the prompt is open. The round-trip + reload run in
    /// a detached `spawn_local` task, so the `new_session` response is not
    /// delayed by the (potentially long) user decision. At most one outstanding
    /// request per workspace per process (dedup), and the await is bounded by
    /// [`TRUST_PROMPT_TIMEOUT`].
    pub(crate) fn maybe_spawn_interactive_trust_prompt(
        &self,
        session_id: &acp::SessionId,
        cwd: &std::path::Path,
        remote: Option<&crate::util::config::RemoteSettings>,
    ) {
        if !self.interactive_trust_client.get() {
            return;
        }
        if !folder_trust::prompt_warranted(cwd, remote) {
            return;
        }
        let key = echo_agent_workspace::trust::workspace_key(cwd);
        // Dedup: skip if this workspace was already prompted/decided (reconnect)
        // or has a prompt in flight (concurrent same-workspace session). `insert`
        // returns false when already present. Agent-owned set (no process
        // global), captured into the task for release on failure/timeout.
        let prompted = self.interactive_trust_prompted.clone();
        if !prompted.borrow_mut().insert(key.clone()) {
            return;
        }

        // Capture EVERY session sharing the GRANTED WORKSPACE (same
        // `workspace_key` — the grant's actual scope, aligned with the dedup key),
        // each with its OWN cwd, so a grant reloads every sibling against its own
        // project config — exactly like the per-cwd `handle_reload_project_mcp_servers`
        // / `broadcast_plugin_registry_to_sessions`. `&self` can't be borrowed
        // across the `spawn_local` boundary, so capture owned clones now.
        //
        // INTENTIONAL fail-safe limitation: this is a one-time snapshot taken at
        // prompt-spawn. A same-workspace session created WHILE the modal is open is
        // deduped (no second prompt) and is not in this set, so the grant won't
        // reload it — it stays GATED until its own next session (secure, never
        // over-exposed). Re-querying at grant time would need the `sessions` map
        // (a non-`Rc` `RefCell` field) shared into the detached task, which isn't
        // available here; the fail-safe stale-session window is accepted instead.
        let mut targets = Vec::new();
        self.session_registry.for_each_resident(|_, h| {
            if echo_agent_workspace::trust::workspace_key(std::path::Path::new(&h.info.cwd)) == key {
                targets.push(ReloadTarget {
                    cmd_tx: h.cmd_tx.clone(),
                    initial_client_mcp_servers: h.initial_client_mcp_servers.clone(),
                    cwd: PathBuf::from(&h.info.cwd),
                });
            }
        });
        if targets.is_empty() {
            prompted.borrow_mut().remove(&key);
            return;
        }

        let gateway = self.gateway.clone();
        let plugin_handle = self.plugin_registry_handle.clone();
        let compat = self.cfg.borrow().compat_resolved;
        let remote = remote.cloned();
        let cwd = cwd.to_path_buf();
        let workspace = key.display().to_string();
        let config_kinds = folder_trust::detected_config_kinds(&cwd);
        let session_id = session_id.0.to_string();
        // Regression guard: every reverse-request must carry a
        // non-empty sessionId or leader Tier-2 routing silently drops it.
        debug_assert!(
            !session_id.is_empty(),
            "folder_trust reverse-request must carry a non-empty sessionId (design §5.4)"
        );

        tokio::task::spawn_local(async move {
            let request = FolderTrustRequest {
                session_id,
                cwd: cwd.to_string_lossy().into_owned(),
                workspace,
                config_kinds,
            };
            // Non-panicking: a struct of String/Vec<String> can't fail to
            // serialize, but avoid `expect` in prod — bail (and release the dedup
            // key) on the impossible error rather than aborting the task thread.
            let raw_params = match serde_json::value::to_raw_value(&request) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = %e, "folder trust: request serialization failed");
                    prompted.borrow_mut().remove(&key);
                    return;
                }
            };
            let ext_request = acp::ExtRequest::new("echo.agent/folder_trust/request", raw_params.into());

            use agent_client_protocol::Client as _;
            let outcome = match tokio::time::timeout(
                TRUST_PROMPT_TIMEOUT,
                gateway.ext_method(ext_request),
            )
            .await
            {
                // A decodable response carries the user's decision. An
                // undecodable success payload is a client/protocol error, not a
                // decision: stay gated (fail-closed) but release the dedup key so
                // a later session can re-prompt — same as transport/timeout below.
                Ok(Ok(raw)) => match serde_json::from_str::<FolderTrustResponse>(raw.0.get()) {
                    Ok(r) => r.outcome,
                    Err(e) => {
                        tracing::debug!(error = %e, "folder trust: undecodable trust response; staying gated, releasing dedup key");
                        prompted.borrow_mut().remove(&key);
                        return;
                    }
                },
                Ok(Err(e)) => {
                    // Client disconnected / transport error: not a decision —
                    // release the key so a later session can re-prompt.
                    tracing::debug!(error = %e, "folder trust: client trust request failed");
                    prompted.borrow_mut().remove(&key);
                    return;
                }
                Err(_elapsed) => {
                    // Connected but silent past the deadline: stay gated, release
                    // the key so a future session may re-prompt.
                    tracing::info!(
                        cwd = %cwd.display(),
                        "folder trust: no client decision before timeout; staying gated"
                    );
                    prompted.borrow_mut().remove(&key);
                    return;
                }
            };
            if outcome != FolderTrustOutcome::Trust {
                // Decided "reject": keep the dedup key so the user is not
                // re-prompted for this workspace on every reconnect.
                tracing::info!(
                    cwd = %cwd.display(),
                    "folder trust: GUI client declined; workspace stays gated"
                );
                return;
            }

            // Re-check the dedup key before granting. `HooksAction::Untrust`
            // removes this workspace's key (and revokes asynchronously) when the
            // user untrusts. If that fired while the modal was open, the key is
            // gone — honor the untrust and drop this now-stale "trust" rather
            // than re-persisting a grant the user just revoked. The single-
            // threaded LocalSet makes this check + grant atomic w.r.t. the
            // untrust task (no await in between).
            if !prompted.borrow().contains(&key) {
                tracing::info!(
                    cwd = %cwd.display(),
                    "folder trust: workspace untrusted while prompt was open; ignoring stale grant"
                );
                return;
            }

            // Persist the grant, then flip the cached untrusted verdict to trusted
            // (the `Some(false)` arm of `resolve_and_record` re-reads the store).
            folder_trust::grant_folder_trust(&cwd);
            folder_trust::resolve_and_record(&cwd, remote.as_ref(), false);

            reload_project_servers_after_grant(ReloadAfterGrant {
                gateway: &gateway,
                targets,
                plugin_handle: &plugin_handle,
                compat: &compat,
                prompt_cwd: &cwd,
            })
            .await;

            tracing::info!(
                cwd = %cwd.display(),
                "folder trust: granted via GUI client; reloaded project servers"
            );
        });
    }
}

/// One session to reload after a grant, with ITS OWN cwd (so the MCP merge +
/// plugin build use the session's own project config — matching the per-cwd
/// canonical reloaders, not the prompt's cwd).
struct ReloadTarget {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::session::SessionCommand>,
    initial_client_mcp_servers: Vec<acp::McpServer>,
    cwd: PathBuf,
}

/// Inputs for [`reload_project_servers_after_grant`], bundled to keep the
/// orchestrator free of a long positional arg list.
struct ReloadAfterGrant<'a> {
    gateway: &'a GatewaySender,
    /// Every session sharing the granted workspace, each with its own cwd.
    targets: Vec<ReloadTarget>,
    plugin_handle: &'a echo_agent_core::plugins::SharedPluginRegistryHandle,
    compat: &'a echo_agent_tools::types::CompatConfig,
    /// The prompting session's cwd — used only for the client catalog push.
    prompt_cwd: &'a std::path::Path,
}

/// Reload each granted-workspace session's now-trusted project servers in place
/// (no restart), driving the canonical primitives the normal spawn/reload paths
/// use — PER SESSION CWD, like `handle_reload_project_mcp_servers` /
/// `broadcast_plugin_registry_to_sessions`: `merge_managed_mcp_servers`
/// (`SessionCommand::UpdateMcpServers`), `build_for_cwd`
/// (`SessionCommand::ReloadPlugins`), and `reload_hooks_impl`
/// (`SessionCommand::ReloadHooks`), then push the refreshed MCP catalog. LSP is
/// spawn-baked and applies on the next session open (see module docs). Caller
/// must have granted + recorded trust first.
async fn reload_project_servers_after_grant(ctx: ReloadAfterGrant<'_>) {
    let plugin_snapshot = ctx.plugin_handle.snapshot();

    for target in ctx.targets {
        // Per-session cwd: a sibling session in a subdir of the granted workspace
        // must get ITS OWN project config, not the prompt's.
        let session_cwd = target.cwd.as_path();
        // MCP: `merge_managed_mcp_servers` re-reads disk + runs
        // `filter_untrusted_project_mcp`, which now KEEPS project servers because
        // the cached verdict was flipped to trusted (same workspace key).
        let _ = crate::session::managed_mcp::merge_and_send_managed_mcp_update(
            &target.cmd_tx,
            session_cwd,
            target.initial_client_mcp_servers,
            plugin_snapshot.as_deref(),
            ctx.compat,
        );
        // Plugins (+ plugin-contributed hooks) built for this session's own cwd
        // on the folder-trust verdict (mirrors `broadcast_plugin_registry_to_sessions`);
        // the grant + resolve_and_record above flipped the cached verdict to trusted.
        let disk_cfg =
            crate::config::resolve_effective_plugins_config(session_cwd).to_discovery_config();
        let project_trusted = folder_trust::project_scope_allowed(session_cwd);
        // Session `_meta.pluginDirs` are re-merged by the receiving actor
        // (`preserve_session_plugin_dirs` on `ReloadPlugins`).
        let registry =
            ctx.plugin_handle
                .build_for_cwd(session_cwd, &disk_cfg, &[], project_trusted);
        let _ = target
            .cmd_tx
            .send(crate::session::SessionCommand::ReloadPlugins { registry });
        // The session's OWN project hooks (`.echo-agent/hooks`, `.cursor/hooks.json`),
        // which `ReloadPlugins` does NOT touch — re-discovered against the actor's
        // own `session_info.cwd` on the now-trusted verdict by `reload_hooks_impl`.
        let _ = target
            .cmd_tx
            .send(crate::session::SessionCommand::ReloadHooks);
    }

    // Push the refreshed MCP catalog (for the prompting session's cwd) so the
    // client UI reflects the now-trusted repo-local servers.
    let local = folder_trust::filter_untrusted_project_mcp(
        ctx.prompt_cwd,
        crate::util::config::load_mcp_servers(ctx.prompt_cwd, ctx.compat),
    );
    crate::extensions::mcp::notify_servers_updated(ctx.gateway, &local).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_with_meta(meta: Option<serde_json::Value>) -> acp::InitializeRequest {
        // Production reads `client_capabilities.meta`, not top-level request meta.
        let mut caps = acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new())
            .terminal(false);
        if let Some(m) = meta
            && let Some(map) = m.as_object().cloned()
        {
            caps = caps.meta(map);
        }
        acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(caps)
    }

    #[test]
    fn parse_interactive_trust_capability_present_and_true() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "echo.agent/folderTrust".to_string(),
            serde_json::json!({ "interactive": true }),
        );
        let init = init_with_meta(Some(serde_json::Value::Object(meta)));
        assert!(MvpAgent::parse_interactive_trust_capability(&init));
    }

    #[test]
    fn parse_interactive_trust_capability_absent_returns_false() {
        let init = init_with_meta(None);
        assert!(!MvpAgent::parse_interactive_trust_capability(&init));
    }

    #[test]
    fn parse_interactive_trust_capability_false_returns_false() {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "echo.agent/folderTrust".to_string(),
            serde_json::json!({ "interactive": false }),
        );
        let init = init_with_meta(Some(serde_json::Value::Object(meta)));
        assert!(!MvpAgent::parse_interactive_trust_capability(&init));
    }

    #[test]
    fn request_serializes_camel_case_with_session_id() {
        let req = FolderTrustRequest {
            session_id: "sess-1".into(),
            cwd: "/repo".into(),
            workspace: "/repo".into(),
            config_kinds: vec!["mcp".into()],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("configKinds").is_some());
        assert!(json.get("config_kinds").is_none());
        // Leader Tier-2 routing reads `params.sessionId`; it must be present and
        // non-empty (regression guard for the silently-dropped-in-leader bug).
        assert_eq!(json["sessionId"], "sess-1");
        assert!(!json["sessionId"].as_str().unwrap().is_empty());
    }

    #[test]
    fn response_decodes_trust_reject_and_unknown_fail_closed() {
        let trust: FolderTrustResponse = serde_json::from_str(r#"{"outcome":"trust"}"#).unwrap();
        assert_eq!(trust.outcome, FolderTrustOutcome::Trust);
        let reject: FolderTrustResponse = serde_json::from_str(r#"{"outcome":"reject"}"#).unwrap();
        assert_eq!(reject.outcome, FolderTrustOutcome::Reject);
        // Unknown outcome must fail closed to Reject (never silently "trust").
        let unknown: FolderTrustResponse = serde_json::from_str(r#"{"outcome":"banana"}"#).unwrap();
        assert_eq!(unknown.outcome, FolderTrustOutcome::Reject);
    }
}
