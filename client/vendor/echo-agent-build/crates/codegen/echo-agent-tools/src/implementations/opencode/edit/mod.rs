//! `edit` tool — OpenCode namespace.
//!
//! Performs exact string replacements in files with support for:
//! - Exact string replacement (find/replace)
//! - New file creation (when `old_string` is empty)
//! - Replace-all mode (`replace_all: true`)
//!
//! Reuses `SearchReplaceOutput` from the output types so that the rest of
//! the crate (prompt rendering, notification routing, etc.) can treat edits
//! from any namespace uniformly.
//!
//! ## Resources
//!
//! - `Cwd` — working directory for path resolution (required)
//! - `FileSystem` — read/write file content (required)
//! - `NotificationHandle` — emit `FileWritten` notifications (required)
//! - `ToolCallId` — notification correlation (required)

use std::sync::Arc;

use crate::computer::types::AsyncFileSystem;
use crate::implementations::echo_agent_build::search_replace::CONTEXT_LINES;
use crate::implementations::echo_agent_build::search_replace::helpers::{
    build_edit_details, render_snippet, replace_using_positions,
};
use crate::notification::types::FileWritten;

use crate::types::output::{
    SearchReplaceEditContextInformation, SearchReplaceEditDetail, SearchReplaceEditsApplied,
    SearchReplaceOutput,
};
use crate::types::requirements::Expr;
#[cfg(test)]
use crate::types::resources::Resources;
#[allow(unused_imports)]
use crate::types::resources::{
    Cwd, DisplayCwd, FileSystem, NotificationHandle, SharedResources, resolve_model_path,
};
use crate::types::tool::{ToolKind, ToolNamespace};

// ───────────────────────────────────────────────────────────────────────────
// Description
// ───────────────────────────────────────────────────────────────────────────

// NOTE: OpenCode's `EditInput` serializes camelCase (`oldString`, `newString`,
// `replaceAll`), so param refs must use the camelCase schema property names —
// the snake_case `params.edit.old_string` keys of the echo_agent_build twin resolve
// to "" here (the kind-params map is keyed by schema property names).
const DESCRIPTION: &str = r#"Performs exact string replacements in files.

Usage:${%- if tools.by_kind.read %}
- When editing text from ${{ tools.by_kind.read }} tool output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix. The line number prefix format is: line number + ": ". Everything after that ": " separator is the actual file content to match. Never include any part of the line number prefix in the ${{ params.edit.oldString }} or ${{ params.edit.newString }}.${%- endif %}
- ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.
- The edit will FAIL if `${{ params.edit.oldString }}` is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use `${{ params.edit.replaceAll }}` to change every instance of `${{ params.edit.oldString }}`.
- Use `${{ params.edit.replaceAll }}` for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance.
- To create a new file, set ${{ params.edit.oldString }} to an empty string.
- Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked."#;

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

/// Input for the opencode `edit` tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditInput {
    /// The path to the file to modify. Can be relative to the workspace or absolute.
    #[schemars(description = "The path to the file to modify.")]
    pub file_path: String,

    /// The text to find in the file. Empty string means create a new file.
    #[schemars(description = "The text to replace")]
    pub old_string: String,

    /// The replacement text (must differ from old_string).
    #[schemars(
        description = "The text to replace it with (must be different from ${{ params.edit.oldString }})"
    )]
    pub new_string: String,

    /// When true, replace every occurrence of `old_string`.
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    #[schemars(description = "Replace all occurrences of ${{ params.edit.oldString }}")]
    pub replace_all: bool,
}

impl TryFrom<crate::types::tool_io::ToolInput> for EditInput {
    type Error = String;
    fn try_from(value: crate::types::tool_io::ToolInput) -> Result<Self, Self::Error> {
        match value {
            crate::types::tool_io::ToolInput::SearchReplace(sr) => Ok(Self {
                file_path: sr.file_path,
                old_string: sr.old_string,
                new_string: sr.new_string,
                replace_all: sr.replace_all,
            }),
            crate::types::tool_io::ToolInput::Dynamic(v) => {
                serde_json::from_value(v).map_err(|e| format!("EditInput: {e}"))
            }
            _ => Err("expected SearchReplace or Dynamic variant for EditInput".into()),
        }
    }
}

// Prefer SearchReplace over Dynamic so AccessKind maps to Edit(path).
impl From<EditInput> for crate::types::tool_io::ToolInput {
    fn from(value: EditInput) -> Self {
        crate::implementations::echo_agent_build::search_replace::SearchReplaceInput {
            file_path: value.file_path,
            old_string: value.old_string,
            new_string: value.new_string,
            replace_all: value.replace_all,
        }
        .into()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tool
// ───────────────────────────────────────────────────────────────────────────

/// OpenCode `edit` tool — performs exact string replacements in files.
#[derive(Debug, Default)]
pub struct EditTool;

impl crate::types::tool_metadata::ToolMetadata for EditTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::OpenCode
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

impl echo_agent_tool_runtime::Tool for EditTool {
    type Args = EditInput;
    type Output = SearchReplaceOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("edit").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "edit",
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

    #[tracing::instrument(
        name = "tool.opencode.edit",
        skip_all,
        fields(
            file_path = %input.file_path,
            replace_all = ?input.replace_all,
        )
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: EditInput,
    ) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::{resolve_cwd, shared_resources};
        let resources = shared_resources(&ctx)?;
        let cwd = resolve_cwd(&ctx, &resources).await?;

        let (display_cwd, fs, notification_handle) = {
            let res = resources.lock().await;
            (
                res.get::<DisplayCwd>().map(|d| d.0.clone()),
                res.require::<FileSystem>()?.0.clone(),
                res.require::<NotificationHandle>()?.0.clone(),
            )
        };
        let tool_call_id = ctx.call_id.as_str().to_owned();

        let replace_all = input.replace_all;

        // Resolve the model-provided path.
        let path = resolve_model_path(&cwd, display_cwd.as_deref(), &input.file_path);

        // ── Validate input ──────────────────────────────────────────
        if path.is_dir() {
            return Ok(SearchReplaceOutput::InvalidInput(
                "File path is a directory".to_owned(),
            ));
        }
        if input.old_string == input.new_string {
            return Ok(SearchReplaceOutput::InvalidInput(
                "Old string and new string are the same".to_owned(),
            ));
        }

        // ── Route to creation or replacement ────────────────────────
        if input.old_string.is_empty() {
            handle_new_file_creation(&input, &fs, &notification_handle, &tool_call_id, &path).await
        } else {
            handle_replacement(
                &input,
                resources.clone(),
                &fs,
                &notification_handle,
                &tool_call_id,
                &path,
                replace_all,
            )
            .await
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Create parent directories for a file path if they don't exist.
async fn ensure_parent_dirs(
    path: &std::path::Path,
) -> Result<(), echo_agent_tool_runtime::ToolError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::execution(
                echo_agent_tool_protocol::ToolId::new("edit").expect("valid"),
                e.to_string(),
            )
        })?;
    }
    Ok(())
}

/// Handle new file creation when `old_string` is empty.
async fn handle_new_file_creation(
    input: &EditInput,
    fs: &Arc<dyn AsyncFileSystem>,
    notification_handle: &crate::notification::types::ToolNotificationHandle,
    tool_call_id: &str,
    path: &std::path::Path,
) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
    // Check if file already exists and is non-empty.
    let file_exists = match fs.read_file(path).await {
        Ok(bytes) => !bytes.is_empty(),
        Err(_) => false,
    };

    if file_exists {
        return Ok(SearchReplaceOutput::FileAlreadyExists(
            "oldString is empty, which is only allowed when creating a new file or when the file is empty.".to_string(),
        ));
    }

    // Create parent directories if needed.
    ensure_parent_dirs(path).await?;

    // Write the new file.
    fs.write_file(path, input.new_string.as_bytes())
        .await
        .map_err(|e| {
            echo_agent_tool_runtime::ToolError::execution(
                echo_agent_tool_protocol::ToolId::new("edit").expect("valid"),
                e.to_string(),
            )
        })?;

    // Emit FileWritten notification.
    notification_handle.send_file_written(FileWritten {
        tool_call_id: tool_call_id.to_string(),
        absolute_path: path.to_path_buf(),
        content: input.new_string.clone(),
        previous_content: None,
        is_new_file: true,
        is_deleted: false,
    });

    // Build output.
    let snippet = input
        .new_string
        .split_inclusive('\n')
        .enumerate()
        .map(|(i, s)| format!("{}→{}", i + 1, s))
        .collect::<String>();

    let tool_output_for_prompt = format!(
        "The file {} has been created. Here's the content:\n\n{snippet}",
        &input.file_path,
    );

    let edits = vec![SearchReplaceEditDetail {
        old_string: input.old_string.clone(),
        old_line: 1,
        new_string: input.new_string.clone(),
        new_line: 1,
        context_before: String::new(),
        context_after: String::new(),
        line_prefix: String::new(),
    }];

    Ok(SearchReplaceOutput::EditsApplied(
        SearchReplaceEditsApplied {
            old_string: input.old_string.clone(),
            new_string: input.new_string.clone(),
            tool_output_for_prompt,
            tool_output_for_prompt_concise: Some(format!(
                "The file {} has been created.",
                &input.file_path
            )),
            absolute_path: path.to_path_buf(),
            edits: SearchReplaceEditContextInformation { details: edits },
            patch: None,
            unicode_normalized: false,
        },
    ))
}

/// Handle replacement in an existing file.
async fn handle_replacement(
    input: &EditInput,
    resources: SharedResources,
    fs: &Arc<dyn AsyncFileSystem>,
    notification_handle: &crate::notification::types::ToolNotificationHandle,
    tool_call_id: &str,
    path: &std::path::Path,
    replace_all: bool,
) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
    // Read current file content.
    let bytes = match fs.read_file(path).await {
        Ok(bytes) => bytes,
        Err(_) if !path.exists() => {
            return Ok(SearchReplaceOutput::FileNotFound(format!(
                "File not found: {}. Please check the path and try again.",
                input.file_path
            )));
        }
        Err(e) => {
            return Err(echo_agent_tool_runtime::ToolError::execution(
                echo_agent_tool_protocol::ToolId::new("edit").expect("valid"),
                e.to_string(),
            ));
        }
    };
    let old_text = String::from_utf8_lossy(&bytes).into_owned();

    // Find all match positions.
    let positions: Vec<usize> = old_text
        .match_indices(&input.old_string)
        .map(|(index, _)| index)
        .collect();

    if positions.is_empty() {
        return Ok(SearchReplaceOutput::NoMatchesFound(
            crate::types::output::NoMatchesFoundError {
                message: "The string to replace was not found in the file. Make sure it matches exactly, including whitespace and indentation.".to_string(),
                file_path: path.to_path_buf(),
                // Same bytes as this `read_file`; consumers must not re-read from disk for hints.
                file_snapshot_at_edit: Some(old_text),
            },
        ));
    }

    if positions.len() > 1 && !replace_all {
        let replace_all_name = crate::types::template_renderer::TemplateRenderer::resolve(
            &resources,
            "${{ params.edit.replaceAll }}",
        )
        .await?;
        return Ok(SearchReplaceOutput::MultipleMatchesFound(format!(
            "The string to replace was found multiple times in the file. Use {} to replace all occurrences, or include more context to only edit one occurrence.",
            replace_all_name
        )));
    }

    // Select which positions to replace.
    let replace_positions = if replace_all {
        &positions[..]
    } else {
        &positions[..1]
    };

    // Perform the replacement using the shared helpers.
    let (new_text, new_positions) = replace_using_positions(
        &old_text,
        replace_positions,
        &input.old_string,
        &input.new_string,
    );

    // Write the updated file.
    fs.write_file(path, new_text.as_bytes())
        .await
        .map_err(|e| {
            echo_agent_tool_runtime::ToolError::execution(
                echo_agent_tool_protocol::ToolId::new("edit").expect("valid"),
                e.to_string(),
            )
        })?;

    // Emit FileWritten notification.
    notification_handle.send_file_written(FileWritten {
        tool_call_id: tool_call_id.to_string(),
        absolute_path: path.to_path_buf(),
        content: new_text.clone(),
        previous_content: Some(old_text.clone()),
        is_new_file: false,
        is_deleted: false,
    });

    // Build edit details using shared helpers.
    let edits = build_edit_details(
        &new_text,
        &input.old_string,
        &input.new_string,
        &new_positions,
        CONTEXT_LINES,
    );

    // Build output message.
    let (tool_output_for_prompt, tool_output_for_prompt_concise) = if new_positions.len() == 1 {
        let (snippet, _, _) = render_snippet(
            &new_text,
            &input.new_string,
            new_positions[0],
            CONTEXT_LINES,
        );
        let default_msg = format!(
            "The file {} has been updated. Here's a relevant snippet of the edited file:\n\n{snippet}",
            &input.file_path,
        );
        let concise_msg = format!("The file {} has been updated.", &input.file_path);
        (default_msg, concise_msg)
    } else {
        let default_msg = format!(
            "All {} occurrences of the specified string were successfully replaced in {}.",
            new_positions.len(),
            &input.file_path,
        );
        let concise_msg = format!(
            "The file {} has been updated. All occurrences were successfully replaced.",
            &input.file_path,
        );
        (default_msg, concise_msg)
    };

    Ok(SearchReplaceOutput::EditsApplied(
        SearchReplaceEditsApplied {
            old_string: input.old_string.clone(),
            new_string: input.new_string.clone(),
            tool_output_for_prompt,
            tool_output_for_prompt_concise: Some(tool_output_for_prompt_concise),
            absolute_path: path.to_path_buf(),
            edits: SearchReplaceEditContextInformation { details: edits },
            patch: None,
            unicode_normalized: false,
        },
    ))
}

// Note: `replace_at_positions`, `render_snippet`, and `build_edit_details`
// are imported from `echo_agent_build::search_replace::helpers` — shared across
// both the echo_agent_build and opencode edit tools.

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::computer::local::LocalFs;
    use crate::notification::types::ToolNotificationHandle;
    use tempfile::TempDir;

    /// Set up Resources with a real filesystem for tests.
    fn test_resources(cwd: &std::path::Path) -> Resources {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;

        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));

        // Keys mirror finalize-time seeding: schema property names, which are
        // camelCase for OpenCode's EditInput.
        let edit_params = std::collections::HashMap::from([
            ("oldString".to_string(), "oldString".to_string()),
            ("newString".to_string(), "newString".to_string()),
            ("replaceAll".to_string(), "replaceAll".to_string()),
        ]);
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "read_file".to_string())]),
            std::collections::HashMap::from([(ToolKind::Edit, edit_params)]),
        ));

        resources
    }

    fn make_input(file_path: &str, old_string: &str, new_string: &str) -> EditInput {
        EditInput {
            file_path: file_path.to_string(),
            old_string: old_string.to_string(),
            new_string: new_string.to_string(),
            replace_all: false,
        }
    }

    #[test]
    fn tool_input_roundtrip_is_search_replace() {
        use crate::types::tool_io::ToolInput;
        let input = make_input("/tmp/denied.txt", "old", "new");
        let tool_input = ToolInput::from(input.clone());
        assert!(matches!(
            tool_input,
            ToolInput::SearchReplace(ref sr) if sr.file_path == "/tmp/denied.txt"
        ));
        let back = EditInput::try_from(tool_input).expect("SearchReplace converts back");
        assert_eq!(back.file_path, "/tmp/denied.txt");
        assert_eq!(back.old_string, "old");
        assert_eq!(back.new_string, "new");
    }

    #[test]
    fn replace_all_defaults_false_and_schema_is_boolean() {
        let missing: EditInput =
            serde_json::from_str(r#"{"filePath":"/f","oldString":"a","newString":"b"}"#).unwrap();
        assert!(!missing.replace_all);

        let nullv: EditInput = serde_json::from_str(
            r#"{"filePath":"/f","oldString":"a","newString":"b","replaceAll":null}"#,
        )
        .unwrap();
        assert!(!nullv.replace_all);

        let schema = serde_json::to_value(schemars::schema_for!(EditInput)).unwrap();
        // rename_all = camelCase → replaceAll
        let p = &schema["properties"]["replaceAll"];
        assert_eq!(p["type"], "boolean", "schema: {schema}");
        assert_eq!(p["default"], false, "schema: {schema}");
        assert!(p.get("anyOf").is_none(), "schema: {schema}");
    }

    // ── Tool metadata ───────────────────────────────────────────────

    #[test]
    fn tool_id_and_kind() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = EditTool;
        assert_eq!(echo_agent_tool_runtime::Tool::id(&tool).as_str(), "edit");
        assert_eq!(tool.kind(), ToolKind::Edit);
        assert!(matches!(tool.tool_namespace(), ToolNamespace::OpenCode));
    }

    #[test]
    fn description_contains_edit_guidance() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = EditTool;
        assert!(
            tool.description_template()
                .contains("exact string replacements")
        );
    }

    // ── Input deserialization ───────────────────────────────────────

    #[test]
    fn input_deserializes_camel_case() {
        let json = serde_json::json!({
            "filePath": "src/main.rs",
            "oldString": "hello",
            "newString": "goodbye",
            "replaceAll": true
        });
        let input: EditInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.file_path, "src/main.rs");
        assert_eq!(input.old_string, "hello");
        assert_eq!(input.new_string, "goodbye");
        assert!(input.replace_all);
    }

    #[test]
    fn input_deserializes_minimal() {
        let json = serde_json::json!({
            "filePath": "test.txt",
            "oldString": "a",
            "newString": "b"
        });
        let input: EditInput = serde_json::from_value(json).unwrap();
        assert_eq!(input.file_path, "test.txt");
        assert!(!input.replace_all);
    }

    // ── Validation ──────────────────────────────────────────────────

    #[tokio::test]
    async fn rejects_same_old_new() {
        let tmp = TempDir::new().unwrap();
        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "same", "same");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert!(msg.contains("same"));
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn rejects_directory() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("subdir", "old", "new");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert!(msg.contains("directory"));
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    // ── Basic replacement ───────────────────────────────────────────

    #[tokio::test]
    async fn basic_replacement() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert_eq!(applied.old_string, "hello");
                assert_eq!(applied.new_string, "goodbye");
                assert!(applied.tool_output_for_prompt.contains("has been updated"));
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "goodbye world\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── New file creation ───────────────────────────────────────────

    #[tokio::test]
    async fn new_file_creation() {
        let tmp = TempDir::new().unwrap();
        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("new_file.txt", "", "new content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(applied.tool_output_for_prompt.contains("has been created"));
                let content = std::fs::read_to_string(tmp.path().join("new_file.txt")).unwrap();
                assert_eq!(content, "new content\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── File already exists (non-empty) ─────────────────────────────

    #[tokio::test]
    async fn file_already_exists_nonempty() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "existing content\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("existing.txt", "", "new content");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::FileAlreadyExists(msg) => {
                assert!(msg.contains("oldString"));
            }
            other => panic!("Expected FileAlreadyExists, got {:?}", other),
        }
    }

    // ── File not found ──────────────────────────────────────────────

    #[tokio::test]
    async fn file_not_found() {
        let tmp = TempDir::new().unwrap();
        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("nonexistent.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::FileNotFound(msg) => {
                assert!(msg.contains("nonexistent.txt"));
            }
            other => panic!("Expected FileNotFound, got {:?}", other),
        }
    }

    // ── No match found ──────────────────────────────────────────────

    #[tokio::test]
    async fn no_match_found() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "xyz", "abc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::NoMatchesFound(ref e) => {
                assert!(e.message.contains("not found"));
                assert_eq!(e.file_snapshot_at_edit.as_deref(), Some("hello world\n"));
            }
            other => panic!("Expected NoMatchesFound, got {:?}", other),
        }
    }

    // ── Multiple matches without replace_all ────────────────────────

    #[tokio::test]
    async fn multiple_matches_without_replace_all() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "aaa", "ccc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::MultipleMatchesFound(msg) => {
                assert!(msg.contains("replaceAll"), "msg: {msg}");
            }
            other => panic!("Expected MultipleMatchesFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn multiple_matches_uses_randomized_param_name() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();

        let tool = EditTool;
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "file_reader".to_string())]),
            std::collections::HashMap::from([(
                ToolKind::Edit,
                // Keyed by the camelCase schema property name (finalize seeds
                // kind params from schema properties).
                std::collections::HashMap::from([(
                    "replaceAll".to_string(),
                    "replaceEverything".to_string(),
                )]),
            )]),
        ));

        let input = make_input("test.txt", "aaa", "ccc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::MultipleMatchesFound(msg) => {
                assert_eq!(
                    msg,
                    "The string to replace was found multiple times in the file. \
                     Use replaceEverything to replace all occurrences, \
                     or include more context to only edit one occurrence."
                );
            }
            other => panic!("Expected MultipleMatchesFound, got {:?}", other),
        }
    }

    // ── Replace all ─────────────────────────────────────────────────

    #[tokio::test]
    async fn replace_all_mode() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa bbb aaa\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = EditInput {
            file_path: "test.txt".to_string(),
            old_string: "aaa".to_string(),
            new_string: "ccc".to_string(),
            replace_all: true,
        };
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "ccc bbb ccc bbb ccc\n");
                assert!(
                    applied
                        .tool_output_for_prompt
                        .contains("successfully replaced")
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Multi-line replacement ────────────────────────────────────

    #[tokio::test]
    async fn multi_line_replacement() {
        let tmp = TempDir::new().unwrap();
        let original = "line1\nline2\nline3\nline4\n";
        std::fs::write(tmp.path().join("test.txt"), original).unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "line2\nline3\n", "replaced_a\nreplaced_b\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "line1\nreplaced_a\nreplaced_b\nline4\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Replacement at end of file ────────────────────────────────

    #[tokio::test]
    async fn replacement_at_end_of_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "first\nsecond\nlast").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "last", "end");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "first\nsecond\nend");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Replacement with different line count ─────────────────────

    #[tokio::test]
    async fn replacement_different_line_count() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "before\nold_line\nafter\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        // Replace 1 line with 3 lines.
        let input = make_input(
            "test.txt",
            "old_line\n",
            "new_line_1\nnew_line_2\nnew_line_3\n",
        );
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(
                    content,
                    "before\nnew_line_1\nnew_line_2\nnew_line_3\nafter\n"
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── New file with nested directories ──────────────────────────

    #[tokio::test]
    async fn new_file_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("a/b/c/new.txt", "", "nested content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(applied.tool_output_for_prompt.contains("has been created"));
                let nested_path = tmp.path().join("a/b/c/new.txt");
                assert!(nested_path.exists(), "Nested file should exist");
                let content = std::fs::read_to_string(&nested_path).unwrap();
                assert_eq!(content, "nested content\n");
                // Parent directories should have been created.
                assert!(tmp.path().join("a/b/c").is_dir());
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Snippet in output ─────────────────────────────────────────

    #[tokio::test]
    async fn snippet_in_output() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "alpha\nbeta\ngamma\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "beta", "BETA");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                // Output should contain line-numbered snippet with → separator.
                assert!(
                    applied.tool_output_for_prompt.contains('→'),
                    "Snippet should contain arrow separator, got: {}",
                    applied.tool_output_for_prompt
                );
                assert!(
                    applied.tool_output_for_prompt.contains("has been updated"),
                    "Should mention file updated"
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Replace-all with three occurrences ────────────────────────

    #[tokio::test]
    async fn replace_all_three_occurrences() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "foo bar foo baz foo\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        let input = EditInput {
            file_path: "test.txt".to_string(),
            old_string: "foo".to_string(),
            new_string: "qux".to_string(),
            replace_all: true,
        };
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "qux bar qux baz qux\n");
                assert!(
                    applied.tool_output_for_prompt.contains("All 3 occurrences"),
                    "Should mention 3 occurrences, got: {}",
                    applied.tool_output_for_prompt
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Empty existing file with old_string="" creates ────────────

    #[tokio::test]
    async fn empty_file_with_empty_old_creates() {
        let tmp = TempDir::new().unwrap();
        // Create an empty file.
        std::fs::write(tmp.path().join("empty.txt"), "").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        // old_string="" on an empty file should succeed (treated as creation).
        let input = make_input("empty.txt", "", "new content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(applied.tool_output_for_prompt.contains("has been created"));
                let content = std::fs::read_to_string(tmp.path().join("empty.txt")).unwrap();
                assert_eq!(content, "new content\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Relative path resolution ──────────────────────────────────

    #[tokio::test]
    async fn relative_path_resolution() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("src");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("lib.rs"), "fn main() {}\n").unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        // Pass a relative path — should resolve against Cwd.
        let input = make_input("src/lib.rs", "fn main() {}", "fn main() { /* edited */ }");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert_eq!(applied.absolute_path, subdir.join("lib.rs"));
                let content = std::fs::read_to_string(subdir.join("lib.rs")).unwrap();
                assert_eq!(content, "fn main() { /* edited */ }\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Edit details populated ──────────────────────────────────

    #[tokio::test]
    async fn edit_details_populated() {
        let tmp = TempDir::new().unwrap();
        let original = "line1\nline2\nline3\nline4\nline5\n";
        std::fs::write(tmp.path().join("test.txt"), original).unwrap();

        let tool = EditTool;
        let resources = test_resources(tmp.path());

        // Replace the middle line.
        let input = make_input("test.txt", "line3\n", "REPLACED\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(
                    !applied.edits.details.is_empty(),
                    "edits.details should not be empty"
                );
                let detail = &applied.edits.details[0];
                assert!(detail.old_line > 0, "old_line should be > 0");
                assert!(detail.new_line > 0, "new_line should be > 0");
                assert!(
                    !detail.context_before.is_empty(),
                    "context_before should be non-empty for a middle-line replacement"
                );
                assert!(
                    !detail.context_after.is_empty(),
                    "context_after should be non-empty for a middle-line replacement"
                );
                assert_eq!(detail.old_string, "line3\n");
                assert_eq!(detail.new_string, "REPLACED\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    // ── Notification sent ───────────────────────────────────────

    // Notification verification requires a capturing handle not available
    // in unit tests. Covered at integration layer.
}
