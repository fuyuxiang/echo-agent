//! Detection of `git commit` / `gh pr create` / `gh pr merge` in terminal
//! commands, shared by the bash tool's counter spans and the shell's PR-metric
//! session signals (the shell inspects `BashOutput.command` / output at its
//! tool-result chokepoint rather than receiving detection through the tool).

/// Git/GitHub operations detected in a successful terminal command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectedGitOps {
    /// A non-dry-run `git commit` statement ran.
    pub committed: bool,
    /// A `gh pr create` statement ran; url/number parsed from output when printed.
    pub pr_created: Option<PrRef>,
    /// A `gh pr merge` statement ran.
    pub pr_merged: bool,
}

/// Reference to a pull request parsed from command output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrRef {
    /// Full PR URL (e.g. `https://github.com/owner/repo/pull/123`).
    pub url: Option<String>,
    /// PR number parsed from the URL path.
    pub number: Option<u64>,
}

impl PrRef {
    /// Find the last `http(s)://…/pull/<N>` URL in `text` — `gh pr create`
    /// stdout, or an MCP create_pull_request result (URLs may be embedded in
    /// JSON strings). Returns `None` when no PR URL is present (e.g.
    /// `gh pr create --web`).
    pub fn find_in(text: &str) -> Option<Self> {
        let mut last = None;
        for (start, _) in text.match_indices("http") {
            let rest = &text[start..];
            if !rest.starts_with("https://") && !rest.starts_with("http://") {
                continue;
            }
            let end = rest
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`')
                })
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
            // rsplit: an owner/repo literally named "pull" must not eat the marker.
            let Some((_, tail)) = url.rsplit_once("/pull/") else {
                continue;
            };
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            let Ok(number) = digits.parse::<u64>() else {
                continue;
            };
            let url_len = url.len() - tail.len() + digits.len();
            last = Some(PrRef {
                url: Some(url[..url_len].to_string()),
                number: Some(number),
            });
        }
        last
    }
}

/// Strip invocation prefixes that precede the actual binary in a statement:
/// `env` (with `-u NAME` args), `VAR=value` assignments, and an absolute /
/// relative path on the binary itself (`/opt/homebrew/bin/gh` → `gh`).
/// Covers common `env` / `VAR=value` / absolute-path wrappers around git/gh.
fn strip_invocation_prefixes(statement: &str) -> &str {
    let mut rest = statement.trim_start();
    loop {
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        let is_env = token == "env";
        let is_env_unset = token == "-u";
        let is_assignment = token.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        });
        if (is_env || is_env_unset || is_assignment) && token_end < rest.len() {
            rest = rest[token_end..].trim_start();
            // `-u` consumes its NAME argument too.
            if is_env_unset {
                let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                if name_end < rest.len() {
                    rest = rest[name_end..].trim_start();
                } else {
                    return rest;
                }
            }
            continue;
        }
        break;
    }
    // Path-invoked binary: keep only the basename token.
    let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    if let Some(slash) = rest[..token_end].rfind('/')
        && matches!(&rest[slash + 1..token_end], "git" | "gh")
    {
        rest = &rest[slash + 1..];
    }
    rest
}

/// Detect `git commit` / `gh pr create` / `gh pr merge` statements in a
/// successful command. Matched per shell statement, anchored at the statement
/// start (after invocation prefixes), so `echo "git commit"`, comments, and
/// `git commit-graph` don't count.
///
/// `output_for_prompt` is scanned for the created PR's URL (`gh pr create`
/// prints it as the last stdout line; absent for `--web`, leaving an empty
/// [`PrRef`]). Callers must only pass exit-code-0 results.
pub fn detect_git_ops(command: &str, output_for_prompt: &str) -> Option<DetectedGitOps> {
    let statements = || {
        command
            .split(['\n', ';', '&', '|'])
            .map(strip_invocation_prefixes)
    };
    // `excluded` guards flags that make the statement a no-op for the metric
    // (`--dry-run` doesn't commit/create; `--disable-auto` un-queues a merge).
    let statement_runs = |prefix: &str, excluded: &str| {
        statements().any(|s| {
            s.strip_prefix(prefix)
                .is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
                && !s.contains(excluded)
        })
    };
    let committed = statement_runs("git commit", "--dry-run");
    let pr_created = statement_runs("gh pr create", "--dry-run")
        .then(|| PrRef::find_in(output_for_prompt).unwrap_or_default());
    let pr_merged = statement_runs("gh pr merge", "--disable-auto");
    (committed || pr_created.is_some() || pr_merged).then_some(DetectedGitOps {
        committed,
        pr_created,
        pr_merged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_and_pr_create_with_url() {
        let out = "exit: 0\nhttps://github.com/example/echo-agent-repo/pull/12345\n";
        let ops = detect_git_ops("git commit -m 'x' && gh pr create --fill", out).unwrap();
        assert!(ops.committed);
        assert!(!ops.pr_merged);
        let pr = ops.pr_created.unwrap();
        assert_eq!(
            pr.url.as_deref(),
            Some("https://github.com/example/echo-agent-repo/pull/12345")
        );
        assert_eq!(pr.number, Some(12345));
    }

    #[test]
    fn pr_create_web_has_no_url() {
        let ops = detect_git_ops("gh pr create --web", "exit: 0\nOpening browser...\n").unwrap();
        let pr = ops.pr_created.unwrap();
        assert_eq!(pr.url, None);
        assert_eq!(pr.number, None);
    }

    #[test]
    fn pr_merge_detected() {
        let ops = detect_git_ops("gh pr merge 42 --squash", "exit: 0\n").unwrap();
        assert!(ops.pr_merged);
        assert!(ops.pr_created.is_none());
        assert!(!ops.committed);
    }

    #[test]
    fn statement_anchoring_rejects_lookalikes() {
        assert!(detect_git_ops(r#"echo "git commit""#, "").is_none());
        assert!(detect_git_ops("git commit-graph write", "").is_none());
        assert!(detect_git_ops(r#"echo "gh pr create is fun""#, "").is_none());
        assert!(detect_git_ops("git commit --dry-run", "").is_none());
        assert!(detect_git_ops("gh pr create --dry-run", "").is_none());
        assert!(detect_git_ops("gh pr merge 42 --disable-auto", "").is_none());
        assert!(detect_git_ops("gh pr view 42", "").is_none());
        assert!(detect_git_ops("grep 'gh pr create' transcript.txt", "").is_none());
    }

    #[test]
    fn multi_statement_split() {
        let ops = detect_git_ops("cd /repo; git commit -am wip", "").unwrap();
        assert!(ops.committed);
        assert!(detect_git_ops("ls | grep foo", "").is_none());
    }

    // Representative invocation-prefix shapes (env vars, absolute paths, env -u).
    #[test]
    fn invocation_prefixes_are_stripped() {
        for cmd in [
            r#"GH_TOKEN="$GITHUB_TOKEN_FORGE" gh pr create --repo o/r --base main"#,
            "NO_COLOR=1 CLICOLOR_FORCE= FORCE_COLOR= gh pr create --fill",
            "/usr/local/bin/gh pr create --head my-branch",
            "/opt/homebrew/bin/gh pr create --fill",
            "env -u GITHUB_TOKEN gh pr create --base main",
            "cd /repo && GIT_AUTHOR_NAME=x /usr/bin/git commit -m msg",
        ] {
            assert!(detect_git_ops(cmd, "").is_some(), "should match: {cmd}");
        }
    }

    #[test]
    fn prefix_stripping_does_not_overreach() {
        // Assignment-only statements and non-git/gh path binaries don't match.
        assert!(detect_git_ops("FOO=gh pr create", "").is_none());
        assert!(detect_git_ops("/usr/bin/echo gh pr create", "").is_none());
        assert!(detect_git_ops("env", "").is_none());
    }

    #[test]
    fn pr_ref_find_in_takes_last_url_and_trims_punctuation() {
        let text = "see https://github.com/o/r/pull/1.\nhttps://ghe.example.test/team/repo/pull/987/files\n";
        let pr = PrRef::find_in(text).unwrap();
        assert_eq!(
            pr.url.as_deref(),
            Some("https://ghe.example.test/team/repo/pull/987")
        );
        assert_eq!(pr.number, Some(987));
    }

    #[test]
    fn pr_ref_find_in_handles_json_embedded_url() {
        let text = r#"{"number":5,"html_url":"https://github.com/o/r/pull/5","state":"open"}"#;
        let pr = PrRef::find_in(text).unwrap();
        assert_eq!(pr.url.as_deref(), Some("https://github.com/o/r/pull/5"));
        assert_eq!(pr.number, Some(5));
    }

    #[test]
    fn pr_ref_find_in_handles_repo_named_pull() {
        let pr = PrRef::find_in("https://github.com/org/pull/pull/7").unwrap();
        assert_eq!(
            pr.url.as_deref(),
            Some("https://github.com/org/pull/pull/7")
        );
        assert_eq!(pr.number, Some(7));
    }

    #[test]
    fn pr_ref_find_in_rejects_non_pr_text() {
        assert_eq!(PrRef::find_in("no urls here"), None);
        assert_eq!(PrRef::find_in("https://github.com/o/r/issues/5"), None);
        assert_eq!(PrRef::find_in("git pull origin main"), None);
        assert_eq!(PrRef::find_in("https://github.com/o/r/pull/"), None);
    }
}
