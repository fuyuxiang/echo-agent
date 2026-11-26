//! `ApplyPatchTool` — Tool trait implementation for the codex apply-patch format.
//!
//! Wires the pure-library patch engine (parser + apply) through `AsyncFileSystem`
//! for all I/O and emits `FileWritten` notifications.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use crate::computer::types::AsyncFileSystem;
use crate::notification::types::FileWritten;

use crate::types::output::{ApplyPatchFileResult, ApplyPatchOutput};
use crate::types::requirements::Expr;
#[allow(unused_imports)]
use crate::types::resources::{Cwd, FileSystem, NotificationHandle, SharedResources};
use crate::types::tool::{ToolKind, ToolNamespace};

use super::errors::ParseError;
use super::parser::{self, Hunk};
use super::{apply, errors::ApplyPatchError};

// ─── Description ─────────────────────────────────────────────────────

/// Tool description derived from the codex `apply_patch_tool_instructions.md`.
const DESCRIPTION: &str = r#"Use this tool to edit files.
Your patch language is a stripped‑down, file‑oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high‑level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. No hunks follow this header.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more “hunks”, each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:
+ for inserted text,
- for removed text, or
  (a space) for unchanged context.

For instructions on [context_before] and [context_after]:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change. If a change is within 3 lines of a previous change, do NOT duplicate the first change’s [context_after] lines in the second change’s [context_before] lines.
- If 3 lines of context is insufficient to uniquely identify the snippet of code within the file, use the @@ operator to indicate the class or function to which the snippet belongs. For instance, we might have:
@@ class BaseClass
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

- If a code block is repeated so many times in a class or function such that even a single `@@` statement and 3 lines of context cannot uniquely identify the snippet of code, you can use multiple `@@` statements to jump to the right context. For instance:

@@ class BaseClass
@@ 	 def method():
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

The full grammar definition is below:
Patch := Begin { FileOp } End
Begin := "*** Begin Patch" NEWLINE
End := "*** End Patch" NEWLINE
FileOp := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" line NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE

A full patch can combine several operations:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

It is important to remember:

- Every patch MUST start with *** Begin Patch and end with *** End Patch, including delete-only patches
- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
"#;

// ─── Input ───────────────────────────────────────────────────────────

/// Input for the `apply_patch` tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ApplyPatchInput {
    /// The patch text in codex patch format.
    pub patch: String,
}

// ─── Tool ────────────────────────────────────────────────────────────

/// ApplyPatch tool — applies multi-file patches in the codex patch format.
#[derive(Debug, Default)]
pub struct ApplyPatchTool;

// ─── Internal types ──────────────────────────────────────────────────

/// A computed file change — all content determined in-memory, ready to write.
enum FileChange {
    Add {
        path: PathBuf,
        content: String,
    },
    Delete {
        path: PathBuf,
        original_content: String,
    },
    Update {
        path: PathBuf,
        original_content: String,
        new_content: String,
    },
    Move {
        source_path: PathBuf,
        dest_path: PathBuf,
        original_content: String,
        new_content: String,
    },
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Create parent directories for a file path if they don't exist.
async fn ensure_parent_dirs(
    path: &std::path::Path,
) -> Result<(), echo_agent_tool_runtime::ToolError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::execution(
                echo_agent_tool_protocol::ToolId::new("apply_patch").expect("valid"),
                e.to_string(),
            )
        })?;
    }
    Ok(())
}

/// Compute all file changes in memory without writing anything.
/// Returns an error string if any hunk can't be applied.
async fn compute_all_changes(
    cwd: &std::path::Path,
    fs: &Arc<dyn AsyncFileSystem>,
    hunks: &[Hunk],
) -> Result<Vec<FileChange>, String> {
    let mut changes = Vec::new();

    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let resolved = cwd.join(path);
                changes.push(FileChange::Add {
                    path: resolved,
                    content: contents.clone(),
                });
            }
            Hunk::DeleteFile { path } => {
                let resolved = cwd.join(path);
                let original_content = read_file_as_string(fs, &resolved)
                    .await
                    .map_err(|e| format!("Failed to read file: {}, {e}", resolved.display()))?;
                changes.push(FileChange::Delete {
                    path: resolved,
                    original_content,
                });
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let resolved = cwd.join(path);
                let original_content = read_file_as_string(fs, &resolved).await.map_err(|e| {
                    format!("Failed to read file to update: {}, {e}", resolved.display())
                })?;

                let new_content = apply::derive_new_contents(&original_content, &resolved, chunks)
                    .map_err(|e| match e {
                        ApplyPatchError::ComputeReplacements(msg) => msg,
                        other => other.to_string(),
                    })?;

                if let Some(dest) = move_path {
                    let resolved_dest = cwd.join(dest);
                    changes.push(FileChange::Move {
                        source_path: resolved,
                        dest_path: resolved_dest,
                        original_content,
                        new_content,
                    });
                } else {
                    changes.push(FileChange::Update {
                        path: resolved,
                        original_content,
                        new_content,
                    });
                }
            }
        }
    }

    Ok(changes)
}

/// Read a file via AsyncFileSystem and convert to String.
async fn read_file_as_string(
    fs: &Arc<dyn AsyncFileSystem>,
    path: &std::path::Path,
) -> Result<String, String> {
    let bytes = fs.read_file(path).await.map_err(|e| format!("{e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Build the codex-style summary string.
fn build_summary(results: &[ApplyPatchFileResult]) -> String {
    let mut out = String::from("Success. Updated the following files:\n");
    for r in results {
        let prefix = match r.action.as_str() {
            "added" => "A",
            "deleted" => "D",
            "moved" => "M",
            _ => "M", // "modified"
        };
        let _ = writeln!(out, "{prefix} {}", r.path.display());
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────

impl crate::types::tool_metadata::ToolMetadata for ApplyPatchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::Codex
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["FileWritten"]
    }

    fn requires_expr(&self) -> Expr<crate::types::requirements::ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for ApplyPatchTool {
    type Args = ApplyPatchInput;
    type Output = ApplyPatchOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("apply_patch").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "apply_patch",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.apply_patch", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: ApplyPatchInput,
    ) -> Result<ApplyPatchOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let (cwd, fs, notification_handle);
        {
            cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;
            let res = resources.lock().await;
            fs = res.require::<FileSystem>()?.0.clone();
            notification_handle = res.require::<NotificationHandle>()?.0.clone();
        }
        let tool_call_id = ctx.call_id.as_str().to_owned();

        // ── Phase 1: Parse ───────────────────────────────────────
        let parsed = match parser::parse_patch(&input.patch) {
            Ok(p) => p,
            Err(e) => {
                let msg = match &e {
                    ParseError::InvalidPatchError(m) => format!("Invalid patch: {m}"),
                    ParseError::InvalidHunkError {
                        message,
                        line_number,
                    } => format!("Invalid patch hunk on line {line_number}: {message}"),
                };
                return Ok(ApplyPatchOutput::ParseError(msg));
            }
        };

        if parsed.hunks.is_empty() {
            return Ok(ApplyPatchOutput::EmptyPatch(
                "No files were modified.".to_string(),
            ));
        }

        // ── Phase 2: Compute all changes in memory (no writes yet) ───
        let changes = match compute_all_changes(&cwd, &fs, &parsed.hunks).await {
            Ok(c) => c,
            Err(msg) => return Ok(ApplyPatchOutput::ApplicationError(msg)),
        };

        // ── Phase 3: Apply all changes (write to filesystem) ─────
        let mut file_results = Vec::new();

        for change in &changes {
            match change {
                FileChange::Add { path, content } => {
                    // Create parent directories if needed.
                    ensure_parent_dirs(path).await?;
                    fs.write_file(path, content.as_bytes()).await.map_err(|e| {
                        echo_agent_tool_runtime::ToolError::execution(
                            echo_agent_tool_protocol::ToolId::new("apply_patch").expect("valid"),
                            e.to_string(),
                        )
                    })?;

                    notification_handle.send_file_written(FileWritten {
                        tool_call_id: tool_call_id.clone(),
                        absolute_path: path.clone(),
                        content: content.clone(),
                        previous_content: None,
                        is_new_file: true,
                        is_deleted: false,
                    });

                    file_results.push(ApplyPatchFileResult {
                        path: path.clone(),
                        action: "added".to_string(),
                        old_text: None,
                        new_text: content.clone(),
                        move_to: None,
                    });
                }
                FileChange::Delete {
                    path,
                    original_content,
                } => {
                    fs.delete_file(path).await.map_err(|e| {
                        echo_agent_tool_runtime::ToolError::execution(
                            echo_agent_tool_protocol::ToolId::new("apply_patch").expect("valid"),
                            e.to_string(),
                        )
                    })?;

                    notification_handle.send_file_written(FileWritten {
                        tool_call_id: tool_call_id.clone(),
                        absolute_path: path.clone(),
                        content: String::new(),
                        previous_content: Some(original_content.clone()),
                        is_new_file: false,
                        is_deleted: true,
                    });

                    file_results.push(ApplyPatchFileResult {
                        path: path.clone(),
                        action: "deleted".to_string(),
                        old_text: Some(original_content.clone()),
                        new_text: String::new(),
                        move_to: None,
                    });
                }
                FileChange::Update {
                    path,
                    original_content,
                    new_content,
                } => {
                    fs.write_file(path, new_content.as_bytes())
                        .await
                        .map_err(|e| {
                            echo_agent_tool_runtime::ToolError::execution(
                                echo_agent_tool_protocol::ToolId::new("apply_patch")
                                    .expect("valid"),
                                e.to_string(),
                            )
                        })?;

                    notification_handle.send_file_written(FileWritten {
                        tool_call_id: tool_call_id.clone(),
                        absolute_path: path.clone(),
                        content: new_content.clone(),
                        previous_content: Some(original_content.clone()),
                        is_new_file: false,
                        is_deleted: false,
                    });

                    file_results.push(ApplyPatchFileResult {
                        path: path.clone(),
                        action: "modified".to_string(),
                        old_text: Some(original_content.clone()),
                        new_text: new_content.clone(),
                        move_to: None,
                    });
                }
                FileChange::Move {
                    source_path,
                    dest_path,
                    original_content,
                    new_content,
                } => {
                    // Create parent dirs for destination.
                    ensure_parent_dirs(dest_path).await?;
                    fs.write_file(dest_path, new_content.as_bytes())
                        .await
                        .map_err(|e| {
                            echo_agent_tool_runtime::ToolError::execution(
                                echo_agent_tool_protocol::ToolId::new("apply_patch")
                                    .expect("valid"),
                                e.to_string(),
                            )
                        })?;
                    // Notify destination (new file at new location).
                    notification_handle.send_file_written(FileWritten {
                        tool_call_id: tool_call_id.clone(),
                        absolute_path: dest_path.clone(),
                        content: new_content.clone(),
                        previous_content: None,
                        is_new_file: true,
                        is_deleted: false,
                    });
                    fs.delete_file(source_path).await.map_err(|e| {
                        echo_agent_tool_runtime::ToolError::execution(
                            echo_agent_tool_protocol::ToolId::new("apply_patch").expect("valid"),
                            e.to_string(),
                        )
                    })?;
                    // Notify source (deleted).
                    notification_handle.send_file_written(FileWritten {
                        tool_call_id: tool_call_id.clone(),
                        absolute_path: source_path.clone(),
                        content: String::new(),
                        previous_content: Some(original_content.clone()),
                        is_new_file: false,
                        is_deleted: true,
                    });

                    file_results.push(ApplyPatchFileResult {
                        path: source_path.clone(),
                        action: "moved".to_string(),
                        old_text: Some(original_content.clone()),
                        new_text: new_content.clone(),
                        move_to: Some(dest_path.clone()),
                    });
                }
            }
        }

        // ── Phase 4: Build summary ───────────────────────────────
        let tool_output_for_prompt = build_summary(&file_results);

        Ok(ApplyPatchOutput::Success {
            files: file_results,
            tool_output_for_prompt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::local::LocalFs;
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx;
    use tempfile::TempDir;

    /// Set up Resources with real filesystem for tests.
    fn test_resources(cwd: &std::path::Path) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources
    }

    /// Build a runtime `ToolCallContext` with the given shared resources.
    fn make_input(patch: &str) -> ApplyPatchInput {
        ApplyPatchInput {
            patch: patch.to_string(),
        }
    }

    fn wrap_patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}\n*** End Patch")
    }

    // ── Add file ─────────────────────────────────────────────────

    #[tokio::test]
    async fn add_file_creates_with_correct_content() {
        let tmp = TempDir::new().unwrap();
        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = wrap_patch("*** Add File: new.txt\n+hello\n+world");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(&patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::Success {
                files,
                tool_output_for_prompt,
            } => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].action, "added");
                assert!(tool_output_for_prompt.contains("A "));
                let content = std::fs::read_to_string(tmp.path().join("new.txt")).unwrap();
                assert_eq!(content, "hello\nworld\n");
            }
            other => panic!("Expected Success, got: {other:?}"),
        }
    }

    // ── Delete file ──────────────────────────────────────────────

    #[tokio::test]
    async fn delete_file_removes_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("del.txt"), "content").unwrap();

        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = wrap_patch("*** Delete File: del.txt");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(&patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::Success {
                files,
                tool_output_for_prompt,
            } => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].action, "deleted");
                assert!(tool_output_for_prompt.contains("D "));
                assert!(!tmp.path().join("del.txt").exists());
            }
            other => panic!("Expected Success, got: {other:?}"),
        }
    }

    // ── Update file ──────────────────────────────────────────────

    #[tokio::test]
    async fn update_file_modifies_content() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("update.txt"), "foo\nbar\n").unwrap();

        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = wrap_patch("*** Update File: update.txt\n@@\n foo\n-bar\n+baz");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(&patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::Success {
                files,
                tool_output_for_prompt,
            } => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].action, "modified");
                assert!(tool_output_for_prompt.contains("M "));
                let content = std::fs::read_to_string(tmp.path().join("update.txt")).unwrap();
                assert_eq!(content, "foo\nbaz\n");
            }
            other => panic!("Expected Success, got: {other:?}"),
        }
    }

    // ── Move file ────────────────────────────────────────────────

    #[tokio::test]
    async fn move_file_renames_and_modifies() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("src.txt"), "line\n").unwrap();

        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = wrap_patch("*** Update File: src.txt\n*** Move to: dst.txt\n@@\n-line\n+line2");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(&patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::Success { files, .. } => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].action, "moved");
                assert_eq!(files[0].move_to, Some(tmp.path().join("dst.txt")));
                assert!(!tmp.path().join("src.txt").exists());
                let content = std::fs::read_to_string(tmp.path().join("dst.txt")).unwrap();
                assert_eq!(content, "line2\n");
            }
            other => panic!("Expected Success, got: {other:?}"),
        }
    }

    // ── Multiple files in one patch ──────────────────────────────

    #[tokio::test]
    async fn multiple_files_in_one_patch() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "old\n").unwrap();

        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = wrap_patch(
            "*** Add File: a.txt\n+aaa\n\
             *** Add File: b.txt\n+bbb",
        );
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(&patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::Success {
                files,
                tool_output_for_prompt,
            } => {
                assert_eq!(files.len(), 2);
                assert!(tool_output_for_prompt.contains("A "));
                assert_eq!(
                    std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
                    "aaa\n"
                );
                assert_eq!(
                    std::fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
                    "bbb\n"
                );
            }
            other => panic!("Expected Success, got: {other:?}"),
        }
    }

    // ── Parse error ──────────────────────────────────────────────

    #[tokio::test]
    async fn parse_error_returns_no_changes() {
        let tmp = TempDir::new().unwrap();
        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(shared.clone()),
            make_input("not a valid patch"),
        )
        .await
        .unwrap();

        match result {
            ApplyPatchOutput::ParseError(msg) => {
                assert!(msg.contains("Invalid patch"));
            }
            other => panic!("Expected ParseError, got: {other:?}"),
        }
    }

    // ── Application error ────────────────────────────────────────

    #[tokio::test]
    async fn application_error_on_missing_lines() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "actual\n").unwrap();

        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = wrap_patch("*** Update File: file.txt\n@@\n-nonexistent\n+replacement");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(&patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::ApplicationError(msg) => {
                assert!(msg.contains("Failed to find expected lines"));
            }
            other => panic!("Expected ApplicationError, got: {other:?}"),
        }
    }

    // ── Empty patch ──────────────────────────────────────────────

    #[tokio::test]
    async fn empty_patch_returns_empty_patch_output() {
        let tmp = TempDir::new().unwrap();
        let tool = ApplyPatchTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();

        let patch = "*** Begin Patch\n*** End Patch";
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), make_input(patch))
                .await
                .unwrap();

        match result {
            ApplyPatchOutput::EmptyPatch(msg) => {
                assert!(msg.contains("No files were modified"));
            }
            other => panic!("Expected EmptyPatch, got: {other:?}"),
        }
    }
}
