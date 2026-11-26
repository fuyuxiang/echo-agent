//! Telling a server which solution or projects to load.
//!
//! Most servers work out what to analyze from `rootUri`. A few load their
//! workspace through a protocol extension instead — Roslyn is the notable one:
//! left alone it treats every file as a loose "miscellaneous file" and reports
//! no project-level diagnostics at all. These notifications are vendor
//! extensions, not LSP, which is why they are modelled here rather than coming
//! from `lsp_types`.

use std::path::Path;

use async_lsp::lsp_types;

use super::config::LspServerConfig;
use super::file_uri;

/// `solution/open`, a Roslyn protocol extension.
enum SolutionOpen {}
impl lsp_types::notification::Notification for SolutionOpen {
    type Params = serde_json::Value;
    const METHOD: &'static str = "solution/open";
}

/// `project/open`, the multi-project counterpart to [`SolutionOpen`].
enum ProjectOpen {}
impl lsp_types::notification::Notification for ProjectOpen {
    type Params = serde_json::Value;
    const METHOD: &'static str = "project/open";
}

/// Tell the server which solution or projects to load. No-op unless configured.
pub fn send(
    server_name: &str,
    config: &LspServerConfig,
    workspace_root: &Path,
    server: &mut async_lsp::ServerSocket,
) {
    let Some(open) = config.workspace_open.as_ref() else {
        return;
    };

    if let Some(solution) = open.solution.as_deref()
        && let Some(uri) = resolve(server_name, workspace_root, solution)
    {
        tracing::info!(server = %server_name, %uri, "solution/open");
        if let Err(e) = server.notify::<SolutionOpen>(serde_json::json!({ "solution": uri })) {
            tracing::warn!(server = %server_name, error = %e, "failed to send solution/open");
        }
    }

    let projects: Vec<String> = open
        .projects
        .iter()
        .filter_map(|project| resolve(server_name, workspace_root, project))
        .collect();
    if !projects.is_empty() {
        tracing::info!(server = %server_name, count = projects.len(), "project/open");
        if let Err(e) = server.notify::<ProjectOpen>(serde_json::json!({ "projects": projects })) {
            tracing::warn!(server = %server_name, error = %e, "failed to send project/open");
        }
    }
}

/// Resolve a configured path against the workspace root and turn it into a URI.
///
/// A path that does not exist is still sent — the server may create or find it
/// — but it is by far the most likely reason for "I configured this and got no
/// diagnostics", so it is worth saying out loud.
fn resolve(server_name: &str, workspace_root: &Path, raw: &str) -> Option<String> {
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };

    match file_uri(&absolute) {
        Ok(uri) => {
            if !absolute.exists() {
                tracing::warn!(
                    server = %server_name,
                    path = %absolute.display(),
                    "workspaceOpen path does not exist; the server will have nothing to load"
                );
            }
            Some(uri.to_string())
        }
        Err(_) => {
            tracing::warn!(
                server = %server_name,
                path = %absolute.display(),
                "workspaceOpen path is not a valid file URI"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_resolve_against_the_workspace_root() {
        let root = tempfile::tempdir().unwrap();
        let solution = root.path().join("MyApp.sln");
        std::fs::write(&solution, "").unwrap();

        let uri = resolve("test", root.path(), "MyApp.sln").expect("should resolve");
        assert!(uri.starts_with("file://"), "{uri}");
        assert!(uri.ends_with("MyApp.sln"), "{uri}");
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let solution = elsewhere.path().join("Other.sln");
        std::fs::write(&solution, "").unwrap();

        let uri =
            resolve("test", root.path(), &solution.to_string_lossy()).expect("should resolve");
        assert!(uri.ends_with("Other.sln"), "{uri}");
        assert!(
            !uri.contains(root.path().to_string_lossy().as_ref()),
            "an absolute path must not be joined to the root: {uri}"
        );
    }

    #[test]
    fn a_missing_path_still_resolves() {
        let root = tempfile::tempdir().unwrap();
        // Warns, but the server is still told — we are not the authority on
        // what it can load.
        assert!(resolve("test", root.path(), "Nope.sln").is_some());
    }
}
