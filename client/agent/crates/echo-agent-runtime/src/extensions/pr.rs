use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use super::{ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrStatusRequest {
    pub cwd: String,
    pub branch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrStatusResponse {
    pub pr: Option<PrData>,
    pub updated_session_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrData {
    pub url: String,
    pub state: String,
    pub is_in_merge_queue: bool,
    pub number: Option<u64>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrViewResponse {
    state: Option<String>,
    url: Option<String>,
    is_draft: Option<bool>,
    number: Option<u64>,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhGraphqlResponse {
    data: Option<GhGraphqlData>,
}

#[derive(Debug, Deserialize)]
struct GhGraphqlData {
    resource: Option<GhGraphqlPullRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhGraphqlPullRequest {
    is_in_merge_queue: Option<bool>,
}

pub async fn handle(_agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/pr/status" => {
            let req = parse_params::<PrStatusRequest>(args)?;
            to_ext_response(handle_pr_status(&req.cwd, &req.branch).await)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_pr_status(cwd: &str, branch: &str) -> anyhow::Result<PrStatusResponse> {
    Ok(PrStatusResponse {
        pr: gh_pr_view_by_branch(cwd, branch).await,
        updated_session_ids: Vec::new(),
    })
}

async fn gh_pr_view_by_branch(cwd: &str, branch: &str) -> Option<PrData> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "pr",
        "view",
        branch,
        "--json",
        "state,url,isDraft,number,title",
    ])
    .current_dir(cwd)
    .stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    // gh colorizes even piped --json output under CLICOLOR_FORCE or
    // GH_FORCE_TTY (inherited from terminal-launched dev environments), and
    // forcing beats NO_COLOR in gh's precedence; there is no --no-color flag
    // (cli/cli#9436). CLICOLOR_FORCE=0 is gh's documented off-switch.
    cmd.env("NO_COLOR", "1");
    cmd.env("CLICOLOR_FORCE", "0");
    cmd.env_remove("GH_FORCE_TTY");
    let output = cmd.output().await.ok()?;

    if !output.status.success() {
        return None;
    }

    let parsed =
        serde_json::from_slice::<GhPrViewResponse>(&strip_ansi_csi(&output.stdout)).ok()?;
    let url = parsed.url?;
    let state = match parsed
        .state
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("merged") => "merged",
        Some("closed") => "closed",
        _ if parsed.is_draft.unwrap_or(false) => "draft",
        _ => "open",
    };
    let is_in_merge_queue = state == "open" && gh_pr_is_in_merge_queue(cwd, &url).await;

    Some(PrData {
        url,
        state: state.to_string(),
        is_in_merge_queue,
        number: parsed.number,
        title: parsed.title,
    })
}

/// `gh pr view --json` does not expose `isInMergeQueue`; query GraphQL via `gh api`.
async fn gh_pr_is_in_merge_queue(cwd: &str, pr_url: &str) -> bool {
    const QUERY: &str =
        "query($url: URI!) { resource(url: $url) { ... on PullRequest { isInMergeQueue } } }";
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "api",
        "graphql",
        "-f",
        &format!("query={QUERY}"),
        "-f",
        &format!("url={pr_url}"),
    ])
    .current_dir(cwd)
    .stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    // Forcing (CLICOLOR_FORCE/GH_FORCE_TTY) beats NO_COLOR in gh's precedence.
    cmd.env("NO_COLOR", "1");
    cmd.env("CLICOLOR_FORCE", "0");
    cmd.env_remove("GH_FORCE_TTY");
    let output = match cmd.output().await {
        Ok(output) => output,
        Err(_) => return false,
    };
    if !output.status.success() {
        let stderr_snippet: String = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(200)
            .collect();
        tracing::warn!(
            status = %output.status,
            stderr = %stderr_snippet,
            "gh api graphql isInMergeQueue lookup failed"
        );
        return false;
    }
    parse_is_in_merge_queue(&output.stdout).unwrap_or(false)
}

fn parse_is_in_merge_queue(stdout: &[u8]) -> Option<bool> {
    let stripped = strip_ansi_csi(stdout);
    let parsed = match serde_json::from_slice::<GhGraphqlResponse>(&stripped) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(error = %error, "failed to parse gh api graphql isInMergeQueue response");
            return None;
        }
    };
    parsed.data?.resource?.is_in_merge_queue
}

/// `gh` can colorize stdout even when piped (e.g. `GH_FORCE_TTY`, `--color always`
/// in config), which would break serde parsing of the JSON payload.
fn strip_ansi_csi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_pr_view_json_parses_after_stripping_forced_color() {
        let stdout = b"\x1b[1;37m{\x1b[m\n  \x1b[1;34m\"isDraft\"\x1b[m\x1b[1;37m:\x1b[m \x1b[33mfalse\x1b[m\x1b[1;37m,\x1b[m\n  \x1b[1;34m\"number\"\x1b[m\x1b[1;37m:\x1b[m 242682\x1b[1;37m,\x1b[m\n  \x1b[1;34m\"state\"\x1b[m\x1b[1;37m:\x1b[m \x1b[32m\"OPEN\"\x1b[m\x1b[1;37m,\x1b[m\n  \x1b[1;34m\"title\"\x1b[m\x1b[1;37m:\x1b[m \x1b[32m\"t\"\x1b[m\x1b[1;37m,\x1b[m\n  \x1b[1;34m\"url\"\x1b[m\x1b[1;37m:\x1b[m \x1b[32m\"https://github.com/example/echoagent/pull/242682\"\x1b[m\n\x1b[1;37m}\x1b[m\n";
        let parsed = serde_json::from_slice::<GhPrViewResponse>(&strip_ansi_csi(stdout)).unwrap();
        assert_eq!(parsed.number, Some(242682));
        assert_eq!(parsed.state.as_deref(), Some("OPEN"));
        assert_eq!(
            parsed.url.as_deref(),
            Some("https://github.com/example/echoagent/pull/242682")
        );
    }

    #[test]
    fn parse_is_in_merge_queue_true() {
        let stdout = br#"{"data":{"resource":{"isInMergeQueue":true}}}"#;
        assert_eq!(parse_is_in_merge_queue(stdout), Some(true));
    }

    #[test]
    fn parse_is_in_merge_queue_false() {
        let stdout = br#"{"data":{"resource":{"isInMergeQueue":false}}}"#;
        assert_eq!(parse_is_in_merge_queue(stdout), Some(false));
    }

    #[test]
    fn parse_is_in_merge_queue_missing_resource() {
        let stdout = br#"{"data":{"resource":null}}"#;
        assert_eq!(parse_is_in_merge_queue(stdout), None);
    }

    #[test]
    fn parse_is_in_merge_queue_missing_data() {
        assert_eq!(parse_is_in_merge_queue(b"{}"), None);
    }

    #[test]
    fn parse_is_in_merge_queue_malformed_json() {
        assert_eq!(parse_is_in_merge_queue(b"not json"), None);
    }

    #[test]
    fn parse_is_in_merge_queue_ansi_wrapped_json() {
        let stdout =
            b"\x1b[1;32m{\"data\":{\"resource\":{\"isInMergeQueue\":\x1b[0;36mtrue\x1b[0m}}}\x1b[0m";
        assert_eq!(parse_is_in_merge_queue(stdout), Some(true));
    }

    #[test]
    fn parse_is_in_merge_queue_ansi_wrapped_false() {
        let stdout = b"\x1b[38;5;208m{\"data\":{\"resource\":{\"isInMergeQueue\":false}}}\x1b[0m\n";
        assert_eq!(parse_is_in_merge_queue(stdout), Some(false));
    }
}
