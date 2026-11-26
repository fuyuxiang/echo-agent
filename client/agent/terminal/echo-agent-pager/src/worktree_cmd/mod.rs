mod display;
use agent_client_protocol as acp;
use anyhow::{Result, bail};
use clap::Subcommand;
use echo_agent_acp::acp_send;
use echo_agent_fast_worktree::WorktreeRecord;
/// Read the agent's own report types rather than copies, so a field added
/// there cannot go missing here.
pub use echo_agent_fast_worktree::{DbStats, GcReport, KeptWorktree, RebuildReport};
use echo_agent_runtime::agent::config::Config as AgentConfig;
use std::io::Write;
use tokio_util::sync::CancellationToken;
#[derive(Debug, clap::Args, Clone)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
}
#[derive(Debug, Subcommand, Clone)]
enum WorktreeCommand {
    /// List tracked worktrees
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, value_delimiter = ',')]
        r#type: Vec<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
    },
    /// Show details for a specific worktree
    Show { id_or_path: String },
    /// Remove worktrees
    Rm {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(short, long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove expired worktrees, keeping any whose work would not survive.
    #[command(alias = "prune")]
    Gc {
        /// Report what would be removed without removing it.
        #[arg(long)]
        dry_run: bool,
        /// Expire worktrees idle longer than this, e.g. `7d`. Without it,
        /// nothing expires.
        #[arg(long)]
        max_age: Option<String>,
        /// Skip the live-process and protected-path guards. This does not
        /// override the safety check; use `echoagent worktree rm` for that.
        #[arg(short, long)]
        force: bool,
    },
    /// Database maintenance
    Db {
        #[command(subcommand)]
        command: WorktreeDbCommand,
    },
}
#[derive(Debug, Subcommand, Clone)]
enum WorktreeDbCommand {
    /// Rebuild DB from filesystem scan
    Rebuild,
    /// Show DB statistics
    Stats,
    /// Print DB file path
    Path,
}
pub async fn run(args: WorktreeArgs, agent_config: &AgentConfig) -> Result<()> {
    let cancel = CancellationToken::new();
    echo_agent_telemetry::startup::mark_utility_process();
    let spawned =
        crate::acp::spawn::spawn_echo_agent_shell(agent_config.clone(), &cancel, None).await?;
    let _agent_guard =
        crate::acp::spawn::AgentShutdownGuard::new(cancel.clone(), Some(spawned.thread_handle));
    let _init: acp::InitializeResponse = acp_send(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_capabilities(
                acp::ClientCapabilities::new()
                    .fs(acp::FileSystemCapabilities::new())
                    .terminal(false),
            )
            .meta(
                serde_json::json!({
                    "clientType": crate::client_identity::HEADLESS_CLIENT_TYPE,
                    "clientVersion": crate::client_identity::PAGER_CLIENT_VERSION
                })
                .as_object()
                .cloned(),
            ),
        &spawned.channel.tx,
    )
    .await?;
    dispatch(args.command, &spawned.channel.tx).await
}
async fn dispatch(command: WorktreeCommand, tx: &echo_agent_acp::AcpAgentTx) -> Result<()> {
    match command {
        WorktreeCommand::List {
            repo,
            r#type,
            json,
            all,
        } => cmd_list(tx, repo, r#type, json, all).await,
        WorktreeCommand::Show { id_or_path } => cmd_show(tx, &id_or_path).await,
        WorktreeCommand::Rm {
            ids,
            force,
            dry_run,
        } => cmd_rm(tx, ids, force, dry_run).await,
        WorktreeCommand::Gc {
            dry_run,
            max_age,
            force,
        } => cmd_gc(tx, dry_run, max_age, force).await,
        WorktreeCommand::Db { command } => cmd_db(tx, command).await,
    }
}
fn ext_request<T: serde::Serialize>(
    method: &str,
    params: &T,
) -> Result<acp::ExtRequest, serde_json::Error> {
    let params = serde_json::value::to_raw_value(params)?;
    Ok(acp::ExtRequest::new(method, params.into()))
}
/// ACP extension responses are wrapped in `{ "result": T, "error": ... }`.
#[derive(serde::Deserialize)]
struct ExtEnvelope<T> {
    result: Option<T>,
    error: Option<serde_json::Value>,
}
async fn ext_call<T: serde::de::DeserializeOwned>(
    tx: &echo_agent_acp::AcpAgentTx,
    method: &str,
    params: &impl serde::Serialize,
) -> Result<T> {
    let req =
        ext_request(method, params).map_err(|e| anyhow::anyhow!("failed to build request: {e}"))?;
    let resp = acp_send(req, tx)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let envelope: ExtEnvelope<T> = serde_json::from_str(resp.0.get())
        .map_err(|e| anyhow::anyhow!("response parse error: {e}"))?;
    if let Some(err) = envelope.error {
        bail!("ACP error: {err}");
    }
    envelope
        .result
        .ok_or_else(|| anyhow::anyhow!("ACP response missing result field"))
}
async fn cmd_list(
    tx: &echo_agent_acp::AcpAgentTx,
    repo: Option<String>,
    types: Vec<String>,
    json: bool,
    all: bool,
) -> Result<()> {
    let records: Vec<WorktreeRecord> = ext_call(
        tx,
        "echo.agent/git/worktree/list",
        &serde_json::json!({
            "repo": repo,
            "type": types,
            "includeAll": all,
        }),
    )
    .await?;
    let mut out = std::io::stdout().lock();
    let written = if json {
        display::print_json(&records, &mut out)
    } else {
        display::print_table(&records, &mut out)
    };
    Ok(crate::util::ignore_broken_pipe(written)?)
}
async fn cmd_show(tx: &echo_agent_acp::AcpAgentTx, id_or_path: &str) -> Result<()> {
    let rec: Option<WorktreeRecord> = ext_call(
        tx,
        "echo.agent/git/worktree/show",
        &serde_json::json!({ "idOrPath" : id_or_path }),
    )
    .await?;
    match rec {
        Some(r) => {
            let written = display::print_show(&r, &mut std::io::stdout().lock());
            Ok(crate::util::ignore_broken_pipe(written)?)
        }
        None => bail!("worktree not found: {id_or_path}"),
    }
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveResponse {
    removed: bool,
    #[serde(default)]
    resolved_path: Option<String>,
}
async fn cmd_rm(
    tx: &echo_agent_acp::AcpAgentTx,
    ids: Vec<String>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    for id_or_path in &ids {
        let resp: Result<RemoveResponse> = ext_call(
            tx,
            "echo.agent/git/worktree/remove",
            &serde_json::json!({
                "idOrPath": id_or_path,
                "force": force,
                "dryRun": dry_run,
            }),
        )
        .await;
        match resp {
            Ok(r) => {
                let path = r.resolved_path.as_deref().unwrap_or(id_or_path);
                if dry_run {
                    println!("  would remove: {path}");
                } else if r.removed {
                    println!("  removed: {path}");
                }
            }
            Err(e) => eprintln!("  error removing {id_or_path}: {e}"),
        }
    }
    Ok(())
}
async fn cmd_gc(
    tx: &echo_agent_acp::AcpAgentTx,
    dry_run: bool,
    max_age: Option<String>,
    force: bool,
) -> Result<()> {
    let report: GcReport = ext_call(
        tx,
        "echo.agent/git/worktree/gc",
        &serde_json::json!({
            "dryRun": dry_run,
            "maxAge": max_age,
            "force": force,
        }),
    )
    .await?;
    let mut out = std::io::stdout().lock();
    let written = (|| {
        if dry_run {
            writeln!(out, "Dry run: no changes made.")?;
        }
        display::print_gc(&report, &mut out)
    })();
    Ok(crate::util::ignore_broken_pipe(written)?)
}
async fn cmd_db(tx: &echo_agent_acp::AcpAgentTx, command: WorktreeDbCommand) -> Result<()> {
    match command {
        WorktreeDbCommand::Stats => {
            let stats: DbStats = ext_call(tx, "echo.agent/git/worktree/db/stats", &()).await?;
            let written = display::print_stats(&stats, &mut std::io::stdout().lock());
            Ok(crate::util::ignore_broken_pipe(written)?)
        }
        WorktreeDbCommand::Path => {
            #[derive(serde::Deserialize)]
            struct PathResp {
                path: String,
            }
            let resp: PathResp = ext_call(tx, "echo.agent/git/worktree/db/path", &()).await?;
            println!("{}", resp.path);
            Ok(())
        }
        WorktreeDbCommand::Rebuild => {
            let report: RebuildReport =
                ext_call(tx, "echo.agent/git/worktree/db/rebuild", &()).await?;
            let written = display::print_rebuild(&report, &mut std::io::stdout().lock());
            Ok(crate::util::ignore_broken_pipe(written)?)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ext_request_builds_list_with_filters() {
        let req = ext_request(
            "echo.agent/git/worktree/list",
            &serde_json::json!({
                "repo": "echoagent",
                "type": ["session"],
                "includeAll": true,
            }),
        )
        .unwrap();
        assert_eq!(req.method.as_ref(), "echo.agent/git/worktree/list");
        let params: serde_json::Value = serde_json::from_str(req.params.get()).unwrap();
        assert_eq!(params["repo"], "echoagent");
        assert_eq!(params["includeAll"], true);
    }
    #[test]
    fn ext_request_builds_gc_with_max_age_string() {
        let req = ext_request(
            "echo.agent/git/worktree/gc",
            &serde_json::json!({
                "dryRun": true,
                "maxAge": "7d",
                "force": false,
            }),
        )
        .unwrap();
        let params: serde_json::Value = serde_json::from_str(req.params.get()).unwrap();
        assert_eq!(params["maxAge"], "7d");
        assert_eq!(params["dryRun"], true);
    }
    #[test]
    fn ext_request_builds_remove_with_id_or_path() {
        let req = ext_request(
            "echo.agent/git/worktree/remove",
            &serde_json::json!({
                "idOrPath": "wt-abc123",
                "force": true,
                "dryRun": false,
            }),
        )
        .unwrap();
        let params: serde_json::Value = serde_json::from_str(req.params.get()).unwrap();
        assert_eq!(params["idOrPath"], "wt-abc123");
    }
    #[test]
    fn ext_request_builds_show() {
        let req = ext_request(
            "echo.agent/git/worktree/show",
            &serde_json::json!({ "idOrPath": "/some/path" }),
        )
        .unwrap();
        let params: serde_json::Value = serde_json::from_str(req.params.get()).unwrap();
        assert_eq!(params["idOrPath"], "/some/path");
    }
    #[test]
    fn ext_request_builds_detach_salvage_clean() {
        let d = ext_request(
            "echo.agent/git/worktree/detach",
            &serde_json::json!({ "idOrPath": "/wt", "allowCopy": false }),
        )
        .unwrap();
        assert_eq!(d.method.as_ref(), "echo.agent/git/worktree/detach");
        let s = ext_request(
            "echo.agent/git/worktree/salvage",
            &serde_json::json!({ "idOrPath": "/wt", "out": "/out" }),
        )
        .unwrap();
        assert_eq!(s.method.as_ref(), "echo.agent/git/worktree/salvage");
        let c = ext_request(
            "echo.agent/git/worktree/clean-artifacts",
            &serde_json::json!({ "idOrPath": "/wt" }),
        )
        .unwrap();
        assert_eq!(c.method.as_ref(), "echo.agent/git/worktree/clean-artifacts");
    }
    #[test]
    fn ext_request_builds_db_stats_empty_params() {
        let req = ext_request("echo.agent/git/worktree/db/stats", &()).unwrap();
        assert_eq!(req.method.as_ref(), "echo.agent/git/worktree/db/stats");
    }
    #[test]
    fn remove_response_deserializes_with_resolved_path() {
        let json = r#"{"removed": true, "resolvedPath": "/resolved"}"#;
        let resp: RemoveResponse = serde_json::from_str(json).unwrap();
        assert!(resp.removed);
        assert_eq!(resp.resolved_path.as_deref(), Some("/resolved"));
    }
    #[test]
    fn remove_response_deserializes_without_resolved_path() {
        let json = r#"{"removed": true}"#;
        let resp: RemoveResponse = serde_json::from_str(json).unwrap();
        assert!(resp.removed);
        assert!(resp.resolved_path.is_none());
    }
    #[test]
    fn ext_envelope_unwraps_success_result() {
        let json = r#"{"result": {"path": "/home/user/.echo-agent/worktrees.db"}, "error": null}"#;
        #[derive(serde::Deserialize)]
        struct PathResp {
            path: String,
        }
        let envelope: ExtEnvelope<PathResp> = serde_json::from_str(json).unwrap();
        assert!(envelope.error.is_none());
        let inner = envelope.result.unwrap();
        assert_eq!(inner.path, "/home/user/.echo-agent/worktrees.db");
    }
    #[test]
    fn ext_envelope_unwraps_error_result() {
        let json = r#"{"result": null, "error": "something went wrong"}"#;
        let envelope: ExtEnvelope<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert!(envelope.result.is_none());
        assert!(envelope.error.is_some());
    }
    #[test]
    fn ext_envelope_unwraps_list_of_records() {
        let json = r#"{"result": [], "error": null}"#;
        let envelope: ExtEnvelope<Vec<WorktreeRecord>> = serde_json::from_str(json).unwrap();
        assert!(envelope.error.is_none());
        assert!(envelope.result.unwrap().is_empty());
    }
    #[test]
    fn ext_envelope_unwraps_db_stats() {
        let json = r#"{"result": {"total_records": 5, "alive_count": 3, "dead_count": 2, "db_file_bytes": 1024}}"#;
        let envelope: ExtEnvelope<DbStats> = serde_json::from_str(json).unwrap();
        let stats = envelope.result.unwrap();
        assert_eq!(stats.total_records, 5);
        assert_eq!(stats.alive_count, 3);
    }
    #[test]
    fn ext_envelope_unwraps_gc_report() {
        let json = r#"{"result": {"dead_removed": 2, "expired_removed": 1, "skipped_alive": 0}}"#;
        let envelope: ExtEnvelope<GcReport> = serde_json::from_str(json).unwrap();
        let report = envelope.result.unwrap();
        assert_eq!(report.dead_removed, 2);
        assert_eq!(report.expired_removed, 1);
        assert_eq!(report.remove_failed, 0);
    }
    /// A worktree the gate kept is not one in use, and a path that was never a
    /// repository is not a worktree that was removed.
    #[test]
    fn kept_worktree_prints_apart_from_a_busy_one_and_from_a_removal() {
        let json = r#"{"result": {"dead_removed": 0, "expired_removed": 3, "skipped_alive": 0,
            "kept_unsafe": 2, "no_repo_paths": 1, "kept_reasons": {"dirty": 2},
            "kept": [{"path": "/wt", "reason": "dirty"}], "not_judged": 4, "unnamed": 5}}"#;
        let envelope: ExtEnvelope<GcReport> = serde_json::from_str(json).unwrap();
        let mut out = Vec::new();
        display::print_gc(&envelope.result.unwrap(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "GC report:",
                "  Dead records removed:      0",
                "  Expired worktrees removed: 3",
                "  Non-repository paths:      1",
                "  Skipped (guarded):         0",
                "  Kept (not reclaimable):    2",
                "    dirty: 2",
                "      /wt  (dirty)",
                "      and 1 more, named in the log",
                "  Not judged this pass:      4",
                "  Naming failed (kept):      5",
            ]
        );
    }
    #[test]
    fn rm_parses_short_force_flag() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: WorktreeCommand,
        }
        let cli = Cli::parse_from(["test", "rm", "-f", "wt-1"]);
        match cli.command {
            WorktreeCommand::Rm {
                ids,
                force,
                dry_run,
            } => {
                assert!(force);
                assert!(!dry_run);
                assert_eq!(ids, vec!["wt-1"]);
            }
            _ => panic!("expected Rm variant"),
        }
    }
    #[test]
    fn rm_parses_long_force_flag() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: WorktreeCommand,
        }
        let cli = Cli::parse_from(["test", "rm", "--force", "a", "b"]);
        match cli.command {
            WorktreeCommand::Rm {
                ids,
                force,
                dry_run,
            } => {
                assert!(force);
                assert!(!dry_run);
                assert_eq!(ids, vec!["a", "b"]);
            }
            _ => panic!("expected Rm variant"),
        }
    }
    #[test]
    fn gc_parses_short_force_flag() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: WorktreeCommand,
        }
        let cli = Cli::parse_from(["test", "gc", "-f"]);
        match cli.command {
            WorktreeCommand::Gc {
                force,
                dry_run,
                max_age,
            } => {
                assert!(force);
                assert!(!dry_run);
                assert!(max_age.is_none());
            }
            _ => panic!("expected Gc variant"),
        }
    }
    #[test]
    fn list_accepts_ls_alias() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: WorktreeCommand,
        }
        let cli = Cli::parse_from(["test", "ls", "--json"]);
        match cli.command {
            WorktreeCommand::List {
                repo,
                r#type,
                json,
                all,
            } => {
                assert!(repo.is_none());
                assert!(r#type.is_empty());
                assert!(json);
                assert!(!all);
            }
            _ => panic!("expected List variant"),
        }
    }
}
