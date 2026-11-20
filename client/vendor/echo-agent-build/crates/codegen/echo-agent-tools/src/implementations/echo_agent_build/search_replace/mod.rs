//! SearchReplace (Edit) tool — new architecture (`Tool` trait).
//!
//! Replaces an exact string in a file, with support for:
//! - New file creation (when `old_string` is empty)
//! - Replace-all mode (`replace_all: true`)
//!
//! ## Resources
//!
//! - `Cwd` — working directory for path resolution (required)
//! - `FileSystem` — read/write file content (required)
//! - `NotificationHandle` — emit `FileWritten` notifications (optional, noop fallback)
//! - `ToolCallId` — notification correlation (optional, defaults empty)
//! - `TemplateRenderer` — resolve client-facing tool/param names in error messages (optional)
pub(crate) mod helpers;
mod versions;
use crate::notification::types::FileWritten;
use crate::types::output::{
    SearchReplaceEditContextInformation, SearchReplaceEditDetail, SearchReplaceEditsApplied,
    SearchReplaceOutput,
};
use crate::types::requirements::{Expr, ToolParamsRequirement, ToolRequirement};
#[allow(unused_imports)]
use crate::types::resources::{
    Cwd, DisplayCwd, FileSystem, GitignoreFilter, NotificationHandle, Params, PathNotFoundHints,
    RespectGitignore, SharedResources, display_cwd_or_cwd, resolve_model_path,
};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::util::truncate_str_with_marker;
use crate::{notification::types::ToolNotificationHandle, register_resource};
use helpers::{
    NormalizedMatchResult, build_edit_details, find_normalized_match_positions,
    replace_normalized_matches, replace_using_positions,
};
pub(crate) const CONTEXT_LINES: usize = 3;
/// Internal version discriminant for search_replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchReplaceVersion {
    Current,
    Legacy0_4_10,
}
impl SearchReplaceVersion {
    pub(crate) fn from_contract(v: Option<&str>) -> Self {
        match v {
            Some("legacy-0.4.10") => Self::Legacy0_4_10,
            _ => Self::Current,
        }
    }
    pub(crate) fn is_legacy(self) -> bool {
        self == Self::Legacy0_4_10
    }
}
/// Full description (for the non-concise toolset).
///
/// Uses MiniJinja template placeholders with ToolKind-based keys:
/// - `${{ tools.by_kind.read }}` — client-facing name for the Read tool
/// - `${{ params.edit.old_string }}` — client-facing param name
/// - `${{ params.edit.replace_all }}` — client-facing param name
pub(crate) const DESCRIPTION_FULL: &str = r#"Replace an exact string in a file.

${% if tools.by_kind.read -%}
- `${{ tools.by_kind.read }}` prefixes each line with "LINE_NUMBER→". That prefix is not part of the file: match only what comes after the →, with its exact indentation.
${% endif -%}
- `${{ params.edit.old_string }}` must match exactly one place in the file. If it appears more than once, add surrounding lines to make it unique, or set `${{ params.edit.replace_all }}` to change every occurrence (handy for renaming an identifier).
- To create a new file, set `${{ params.edit.old_string }}` to an empty string. An empty `${{ params.edit.old_string }}` cannot overwrite an existing non-empty file."#;
/// The overwrite-guard sentence in [`DESCRIPTION_FULL`]. Only accurate while
/// `empty_old_string_does_not_override` is enabled (opt-in; the default is the
/// legacy overwrite behavior); `versioned_definition` strips it unless a
/// config enables the guard.
pub(crate) const EMPTY_OLD_STRING_GUARD_SENTENCE: &str =
    " An empty `${{ params.edit.old_string }}` cannot overwrite an existing non-empty file.";
/// Input for the search_replace tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchReplaceInput {
    #[schemars(
        description = "The path to the file to modify. You can use either a relative path in the workspace or an absolute path."
    )]
    pub file_path: String,
    #[schemars(description = "The text to replace")]
    pub old_string: String,
    #[schemars(
        description = "The text to replace it with (must be different from ${{ params.edit.old_string }})"
    )]
    pub new_string: String,
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    #[schemars(
        description = "Replace all occurrences of ${{ params.edit.old_string }} (default false)"
    )]
    pub replace_all: bool,
}
fn default_true() -> bool {
    true
}
/// Configuration for the search_replace tool, stored as `Params<SearchReplaceParams>` in Resources.
///
/// Replaces the old `SearchReplaceOptions` that was stored via `tool_options_as()`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchReplaceParams {
    /// Deprecated runtime no-op, kept so configs still sending it deserialize under
    /// `deny_unknown_fields`. Still gates the config-time Read-tool requirement (`requires_expr`).
    #[serde(default)]
    pub skip_read_before_edit: bool,
    /// When true (opt-in), an empty `old_string` may only create a new file
    /// or fill an empty one — it never silently overwrites an existing
    /// non-empty file. Defaults to false (the legacy behavior): an empty
    /// `old_string` replaces the file's entire contents. The served
    /// description includes the guard sentence only when this is enabled
    /// (see `versioned_definition`).
    #[serde(default)]
    pub empty_old_string_does_not_override: bool,
    /// When true, enable normalized-fallback matching for Unicode confusable
    /// characters (smart quotes, em-dashes, etc.).  When exact byte matching
    /// fails, the tool will retry with confusable-normalized comparison and
    /// perform the replacement if an unambiguous match is found.
    ///
    /// Default: `false` — disabled until Stage 1 diagnostics are stable.
    #[serde(default)]
    pub unicode_normalized_fallback: bool,
    /// When true, append a hint that the user may have changed the file
    /// to `NoMatchesFound` error messages. This nudges the model to re-read
    /// instead of blindly retrying with the same stale content.
    ///
    /// Default: `true`.
    #[serde(default = "default_true")]
    pub include_user_edit_hint: bool,
}
impl Default for SearchReplaceParams {
    fn default() -> Self {
        Self {
            skip_read_before_edit: false,
            empty_old_string_does_not_override: false,
            unicode_normalized_fallback: false,
            include_user_edit_hint: true,
        }
    }
}
register_resource!("echo_agent_build", "SearchReplace", SearchReplaceParams);
/// SearchReplace tool — new architecture.
///
/// Replaces an exact string in a file.
#[derive(Debug, Default)]
pub struct SearchReplaceTool;
/// Core search-replace logic shared by `SearchReplaceTool` and `SearchReplaceConciseTool`.
///
/// Concise prompt swapping is done by the caller after this returns.
pub(crate) async fn run_search_replace(
    input: SearchReplaceInput,
    ctx: &echo_agent_tool_runtime::ToolCallContext,
    resources: SharedResources,
) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
    let cwd_override = ctx
        .extensions
        .get::<echo_agent_tool_runtime::Cwd>()
        .map(|c| c.0.clone());
    let contract_version = ctx
        .extensions
        .get::<echo_agent_tool_runtime::BehaviorVersion>()
        .map(|v| v.0.clone());
    let tool_call_id = ctx.call_id.as_str().to_owned();
    let (cwd, display_cwd, fs, notification_handle, hints_enabled);
    {
        let res = resources.lock().await;
        cwd = match cwd_override {
            Some(ref dir) => dir.clone(),
            None => res.require::<Cwd>()?.0.clone(),
        };
        display_cwd = res.get::<DisplayCwd>().map(|d| d.0.clone());
        fs = res.require::<FileSystem>()?.0.clone();
        notification_handle = res.require::<NotificationHandle>()?.0.clone();
        hints_enabled = res.get::<PathNotFoundHints>().is_some_and(|h| h.0);
    }
    let resolved = resolve_model_path(&cwd, display_cwd.as_deref(), &input.file_path);
    let path = match crate::util::fs::try_canonicalize(&resolved).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match crate::util::try_resolve_unicode_filename(&resolved).await {
                Some(m) => m.resolved_path,
                None => resolved,
            }
        }
        Err(_) => resolved,
    };
    if let Some(err) = validate_path_length(&input.file_path) {
        return Ok(err);
    }
    if path.is_dir() {
        return Ok(SearchReplaceOutput::InvalidInput(
            "File path is a directory".to_owned(),
        ));
    }
    let is_legacy = SearchReplaceVersion::from_contract(contract_version.as_deref()).is_legacy();
    if !is_legacy {
        let res = resources.lock().await;
        let respect_gitignore = res.get::<RespectGitignore>().is_none_or(|r| r.0);
        if respect_gitignore
            && let Some(filter) = res.get::<GitignoreFilter>()
            && filter.is_ignored(&path)
        {
            return Ok(SearchReplaceOutput::InvalidInput(format!(
                "Error: {} is ignored by .gitignore and cannot be edited.",
                input.file_path
            )));
        }
    }
    if input.old_string == input.new_string {
        return Ok(SearchReplaceOutput::InvalidInput(
            "Old string and new string are the same".to_owned(),
        ));
    }
    let (empty_old_string_does_not_override, include_user_edit_hint);
    {
        let res = resources.lock().await;
        let sr_params = res.get::<Params<SearchReplaceParams>>();
        empty_old_string_does_not_override = sr_params
            .map(|p| p.0.empty_old_string_does_not_override)
            .unwrap_or(false);
        include_user_edit_hint = sr_params
            .map(|p| p.0.include_user_edit_hint)
            .unwrap_or(true);
    }
    let result = if input.old_string.is_empty() {
        handle_new_file_creation(
            &input,
            resources.clone(),
            &fs,
            &notification_handle,
            &tool_call_id,
            &path,
            &cwd,
            display_cwd.as_deref(),
            hints_enabled,
            empty_old_string_does_not_override,
        )
        .await?
    } else {
        handle_replacement(
            &input,
            resources.clone(),
            &fs,
            &notification_handle,
            &tool_call_id,
            &path,
            &cwd,
            display_cwd.as_deref(),
            hints_enabled,
            is_legacy,
            include_user_edit_hint,
        )
        .await?
    };
    if let SearchReplaceOutput::EditsApplied(applied) = &result {
        let (mut added, mut removed) = (0i64, 0i64);
        for detail in &applied.edits.details {
            let (a, r) = crate::types::output::line_diff(&detail.old_string, &detail.new_string);
            added += a;
            removed += r;
        }
        tracing::info_span!(
            "edit.lines",
            tool_name = "search_replace",
            lines_added = added,
            lines_removed = removed
        )
        .in_scope(|| {});
    }
    Ok(result)
}
/// Maximum length for a single path component (file or directory name).
/// POSIX `NAME_MAX` is 255 on both macOS and Linux.
const NAME_MAX: usize = 255;
/// Validate that no path component exceeds `NAME_MAX`.
///
/// Returns `Some(SearchReplaceOutput::FilenameTooLong(..))` if any component is
/// too long, `None` if the path is valid.
fn validate_path_length(file_path: &str) -> Option<SearchReplaceOutput> {
    for component in std::path::Path::new(file_path).components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if name_str.len() > NAME_MAX {
                return Some(SearchReplaceOutput::FilenameTooLong(format!(
                    "Error: file name exceeds the {NAME_MAX}-character limit \
                     ({} characters). Please use a shorter file name.",
                    name_str.len(),
                )));
            }
        }
    }
    None
}
/// Handle new file creation when `old_string` is empty.
async fn handle_new_file_creation(
    input: &SearchReplaceInput,
    resources: SharedResources,
    fs: &std::sync::Arc<dyn crate::computer::types::AsyncFileSystem>,
    notification_handle: &ToolNotificationHandle,
    tool_call_id: &str,
    path: &std::path::Path,
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    hints_enabled: bool,
    empty_old_string_does_not_override: bool,
) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
    let file_exists = match fs.read_file(path).await {
        Ok(bytes) => !bytes.is_empty(),
        Err(_) => false,
    };
    let old_text = match fs.read_file(path).await {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).to_string()),
        Err(_) => None,
    };
    if file_exists && empty_old_string_does_not_override {
        let old_string_name;
        {
            let res = resources.lock().await;
            let renderer = res.require::<TemplateRenderer>()?;
            old_string_name = renderer
                .render("${{ params.edit.old_string }}")
                .map_err(|e| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
                })?;
        }
        return Ok(SearchReplaceOutput::FileAlreadyExists(format!(
            "{} is empty, which is only allowed when creating a new file or when the file is empty.",
            old_string_name
        )));
    }
    if let Err(e) = fs.write_file(path, input.new_string.as_bytes()).await {
        return Ok(match e.io_error_kind() {
            Some(std::io::ErrorKind::NotFound) => {
                let display_dcwd = display_cwd_or_cwd(cwd, display_cwd);
                let display_path = display_dcwd.join(&input.file_path);
                let msg = crate::util::format_not_found_error(
                    &display_path,
                    path,
                    cwd,
                    &display_dcwd,
                    hints_enabled,
                )
                .await;
                SearchReplaceOutput::FileNotFound(msg)
            }
            Some(std::io::ErrorKind::AlreadyExists) => SearchReplaceOutput::InvalidInput(format!(
                "Error: cannot create {}. A component of the path already exists as a file where a directory is expected.",
                input.file_path
            )),
            Some(std::io::ErrorKind::InvalidFilename) => {
                SearchReplaceOutput::FilenameTooLong(format!(
                    "Error: file name exceeds the {NAME_MAX}-character limit. \
                     Please use a shorter file name.",
                ))
            }
            _ => SearchReplaceOutput::InvalidInput(format!(
                "Error: failed to write {}: {e}",
                input.file_path
            )),
        });
    }
    if let Some(old_text) = old_text
        && file_exists
        && !empty_old_string_does_not_override
    {
        notification_handle.send_file_written(FileWritten {
            tool_call_id: tool_call_id.to_string(),
            absolute_path: path.to_path_buf(),
            content: input.new_string.clone(),
            previous_content: Some(old_text.clone()),
            is_new_file: false,
            is_deleted: false,
        });
    } else {
        notification_handle.send_file_written(FileWritten {
            tool_call_id: tool_call_id.to_string(),
            absolute_path: path.to_path_buf(),
            content: input.new_string.clone(),
            previous_content: None,
            is_new_file: true,
            is_deleted: false,
        });
    }
    let tool_output_for_prompt = format!(
        "The file {} has been created successfully.",
        &input.file_path
    );
    let tool_output_for_prompt_concise = format!("The file {} has been created.", &input.file_path);
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
            tool_output_for_prompt_concise: Some(tool_output_for_prompt_concise),
            absolute_path: path.to_path_buf(),
            edits: SearchReplaceEditContextInformation { details: edits },
            patch: None,
            unicode_normalized: false,
        },
    ))
}
/// Return a short nearest-match hint for a `NoMatchesFound` error message.
///
/// Finds the first file line containing the longest token from `old_string`'s
/// first line. Returns `"\n\nNearest match: line N: <content>"` (≤200 chars),
/// or an empty string if no match is found.
fn build_nearest_match_hint(file: &str, old_string: &str) -> String {
    let keyword = old_string
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .max_by_key(|w| w.len())
        .unwrap_or("");
    if keyword.is_empty() {
        return String::new();
    }
    file.lines()
        .enumerate()
        .find(|(_, l)| l.contains(keyword))
        .map(|(i, l)| {
            let full = format!("\n\nNearest match: line {}: {}", i + 1, l.trim_end());
            truncate_str_with_marker(&full, 200).into_owned()
        })
        .unwrap_or_default()
}
/// Build a Unicode-confusable diagnostic message when an exact match fails
/// but the file contains typography characters that may have caused the miss.
///
/// Performs a normalized comparison: if `normalize_confusables(file)` contains
/// `normalize_confusables(old_string)`, the miss was almost certainly caused by
/// invisible Unicode characters.  In that case, returns a targeted diagnostic
/// listing only the confusable-bearing lines that overlap the matched region
/// (not every confusable line in the file).
///
/// Returns `None` when:
/// - The file contains no confusables at all, or
/// - The normalized comparison also fails (confusables are present but unrelated
///   to the missed match — no false guidance).
fn build_confusable_hint(
    file: &str,
    old_string: &str,
    tools: crate::util::query_tools::QueryTools,
    read_tool_name: &str,
    old_string_param: &str,
    execute_tool_name: &str,
) -> Option<String> {
    use crate::util::unicode_confusables::{
        build_offset_map, detect_confusables, normalize_confusables,
    };
    if !crate::util::unicode_confusables::has_confusables(file) {
        return None;
    }
    let (norm_file, offset_map) = build_offset_map(file);
    let norm_old = normalize_confusables(old_string);
    let norm_start = norm_file.find(&norm_old)?;
    let orig_start = offset_map[norm_start];
    let orig_end = offset_map[norm_start + norm_old.len()];
    let match_start_line = file[..orig_start].matches('\n').count() + 1;
    let match_end_line = file[..orig_end].matches('\n').count() + 1;
    let hits = detect_confusables(file);
    let mut affected_lines: Vec<usize> = hits
        .iter()
        .filter(|h| h.line_number >= match_start_line && h.line_number <= match_end_line)
        .map(|h| h.line_number)
        .collect();
    affected_lines.dedup();
    if affected_lines.is_empty() {
        return None;
    }
    const MAX_LISTED_LINES: usize = 8;
    let line_summary = if affected_lines.len() <= MAX_LISTED_LINES {
        affected_lines
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        let shown: Vec<String> = affected_lines[..MAX_LISTED_LINES]
            .iter()
            .map(|n| n.to_string())
            .collect();
        format!(
            "{} (and {} more)",
            shown.join(", "),
            affected_lines.len() - MAX_LISTED_LINES
        )
    };
    let read_qualifier = if read_tool_name.is_empty() {
        String::new()
    } else {
        format!(" in {read_tool_name} output")
    };
    let old_string_param = if old_string_param.is_empty() {
        "old_string"
    } else {
        old_string_param
    };
    let edit_tools = tools.edit_tools();
    let terminal_fallback = if edit_tools.is_empty() || execute_tool_name.is_empty() {
        String::new()
    } else {
        format!(
            ", or use {} with a short script{} to edit the file directly",
            execute_tool_name,
            crate::util::query_tools::examples_clause(&edit_tools)
        )
    };
    Some(format!(
        "\n\nThe nearest matching region contains Unicode typography characters \
         (smart quotes, em-dashes, etc.) on lines {} that look identical to \
         ASCII{} but differ at the byte level. Re-read the file and \
         use a shorter {} anchored on nearby ASCII-only context{}.",
        line_summary, read_qualifier, old_string_param, terminal_fallback
    ))
}
/// Handle replacement in existing file.
async fn handle_replacement(
    input: &SearchReplaceInput,
    resources: SharedResources,
    fs: &std::sync::Arc<dyn crate::computer::types::AsyncFileSystem>,
    notification_handle: &ToolNotificationHandle,
    tool_call_id: &str,
    path: &std::path::Path,
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    hints_enabled: bool,
    is_legacy: bool,
    include_user_edit_hint: bool,
) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
    let bytes = match fs.read_file(path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            let output = match e.io_error_kind() {
                Some(std::io::ErrorKind::NotFound) => {
                    let display_dcwd = display_cwd_or_cwd(cwd, display_cwd);
                    let display_path = display_dcwd.join(&input.file_path);
                    let msg = crate::util::format_not_found_error(
                        &display_path,
                        path,
                        cwd,
                        &display_dcwd,
                        hints_enabled,
                    )
                    .await;
                    SearchReplaceOutput::FileNotFound(msg)
                }
                Some(std::io::ErrorKind::IsADirectory) => SearchReplaceOutput::InvalidInput(
                    format!("Error: {} is a directory, not a file.", input.file_path),
                ),
                Some(std::io::ErrorKind::InvalidFilename) => {
                    SearchReplaceOutput::FilenameTooLong(format!(
                        "Error: file name exceeds the {NAME_MAX}-character limit. \
                         Please use a shorter file name.",
                    ))
                }
                Some(std::io::ErrorKind::PermissionDenied) => SearchReplaceOutput::InvalidInput(
                    format!("Error: permission denied reading {}.", input.file_path),
                ),
                _ => {
                    return Err(echo_agent_tool_runtime::ToolError::execution(
                        echo_agent_tool_protocol::ToolId::new("search_replace").expect("valid"),
                        e.to_string(),
                    ));
                }
            };
            return Ok(output);
        }
    };
    let old_text = String::from_utf8_lossy(&bytes).into_owned();
    let has_crlf = old_text.contains("\r\n");
    let match_text: std::borrow::Cow<'_, str> = if has_crlf {
        std::borrow::Cow::Owned(old_text.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(&old_text)
    };
    let mut positions: Vec<usize> = match_text
        .match_indices(&input.old_string)
        .map(|(index, _)| index)
        .collect();
    let mut used_normalized_fallback = false;
    if positions.is_empty() {
        let fallback_enabled = {
            let res = resources.lock().await;
            res.get::<Params<SearchReplaceParams>>()
                .is_some_and(|p| p.0.unicode_normalized_fallback)
        };
        if fallback_enabled {
            match find_normalized_match_positions(&match_text, &input.old_string) {
                NormalizedMatchResult::Matches(normalized_matches) => {
                    if normalized_matches.len() > 1 && !input.replace_all {
                        let replace_all_name =
                            TemplateRenderer::resolve(&resources, "${{ params.edit.replace_all }}")
                                .await?;
                        return Ok(SearchReplaceOutput::MultipleMatchesFound(format!(
                            "The string to replace was found multiple times in the file \
                             (via Unicode normalization). Use {} to replace all occurrences, \
                             or include more context to only edit one occurrence.",
                            replace_all_name
                        )));
                    }
                    positions = normalized_matches
                        .iter()
                        .map(|m| m.original_start)
                        .collect();
                    used_normalized_fallback = true;
                }
                NormalizedMatchResult::Ambiguous => {
                    let old_string_name =
                        TemplateRenderer::resolve(&resources, "${{ params.edit.old_string }}")
                            .await?;
                    return Ok(SearchReplaceOutput::MultipleMatchesFound(format!(
                        "The string to replace was found via Unicode normalization but the \
                         match is ambiguous (partial or overlapping). Use a more specific \
                         {} that avoids lines with Unicode typography characters.",
                        old_string_name
                    )));
                }
                NormalizedMatchResult::NoMatch => {}
            }
        }
    }
    if positions.is_empty() {
        let (read_name, old_string_param, execute_name) = {
            let res = resources.lock().await;
            let renderer = res.require::<TemplateRenderer>()?;
            let read_name = renderer.render("${{ tools.by_kind.read }}").map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
            })?;
            let old_string_param =
                renderer
                    .render("${{ params.edit.old_string }}")
                    .map_err(|e| {
                        echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
                    })?;
            let execute_name = renderer
                .render("${{ tools.by_kind.execute }}")
                .map_err(|e| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
                })?;
            (read_name, old_string_param, execute_name)
        };
        let hint = if is_legacy {
            String::new()
        } else {
            build_nearest_match_hint(&match_text, &input.old_string)
        };
        let confusable_hint = if is_legacy {
            String::new()
        } else {
            build_confusable_hint(
                &match_text,
                &input.old_string,
                crate::util::query_tools::QueryTools::detect(),
                &read_name,
                &old_string_param,
                &execute_name,
            )
            .unwrap_or_default()
        };
        let user_edit_hint = if include_user_edit_hint {
            " The user may have changed the file since you last read it."
        } else {
            ""
        };
        return Ok(SearchReplaceOutput::NoMatchesFound(
            crate::types::output::NoMatchesFoundError {
                message: format!(
                    "The string to replace was not found in the file, use the {} tool to see the correct string.{}{}{}",
                    read_name, user_edit_hint, hint, confusable_hint
                ),
                file_path: path.to_path_buf(),
                file_snapshot_at_edit: None,
            },
        ));
    }
    if positions.len() > 1 && !input.replace_all {
        let replace_all_name =
            TemplateRenderer::resolve(&resources, "${{ params.edit.replace_all }}").await?;
        return Ok(SearchReplaceOutput::MultipleMatchesFound(format!(
            "The string to replace was found multiple times in the file. Use {} to replace all occurrences, or include more context to only edit one occurrence.",
            replace_all_name
        )));
    }
    let (new_text, new_positions) = if used_normalized_fallback {
        let normalized_matches =
            match find_normalized_match_positions(&match_text, &input.old_string) {
                NormalizedMatchResult::Matches(m) => m,
                _ => {
                    return Ok(SearchReplaceOutput::NoMatchesFound(
                        crate::types::output::NoMatchesFoundError {
                            message:
                                "Internal error: normalized match disappeared on re-evaluation"
                                    .to_string(),
                            file_path: path.to_path_buf(),
                            file_snapshot_at_edit: None,
                        },
                    ));
                }
            };
        replace_normalized_matches(&match_text, &normalized_matches, &input.new_string)
    } else {
        replace_using_positions(
            &match_text,
            &positions,
            &input.old_string,
            &input.new_string,
        )
    };
    let write_text = if has_crlf {
        new_text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        new_text.clone()
    };
    if let Err(e) = fs.write_file(path, write_text.as_bytes()).await {
        return Ok(match e.io_error_kind() {
            Some(std::io::ErrorKind::AlreadyExists) => SearchReplaceOutput::InvalidInput(format!(
                "Error: cannot write {}. A component of the path already exists as a file where a directory is expected.",
                input.file_path
            )),
            Some(std::io::ErrorKind::InvalidFilename) => {
                SearchReplaceOutput::FilenameTooLong(format!(
                    "Error: file name exceeds the {NAME_MAX}-character limit. Please use a shorter file name."
                ))
            }
            _ => SearchReplaceOutput::InvalidInput(format!(
                "Error: failed to write {}: {e}",
                input.file_path
            )),
        });
    }
    notification_handle.send_file_written(FileWritten {
        tool_call_id: tool_call_id.to_string(),
        absolute_path: path.to_path_buf(),
        content: write_text.clone(),
        previous_content: Some(old_text.clone()),
        is_new_file: false,
        is_deleted: false,
    });
    let edits = build_edit_details(
        &new_text,
        &input.old_string,
        &input.new_string,
        &new_positions,
        CONTEXT_LINES,
    );
    let (tool_output_for_prompt, tool_output_for_prompt_concise) = if new_positions.len() == 1 {
        let default_msg = format!(
            "The file {} has been updated successfully.",
            &input.file_path
        );
        let concise_msg = format!("The file {} has been updated.", &input.file_path);
        (default_msg, concise_msg)
    } else {
        let default_msg = format!(
            "The file {} has been updated. All occurrences were successfully replaced.",
            &input.file_path
        );
        let concise_msg = format!(
            "The file {} has been updated. All occurrences were replaced.",
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
            unicode_normalized: used_normalized_fallback,
        },
    ))
}
impl crate::types::tool_metadata::ToolMetadata for SearchReplaceTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }
    fn description_template(&self) -> &str {
        DESCRIPTION_FULL
    }
    /// Params-aware description: the "cannot overwrite" sentence in
    /// [`DESCRIPTION_FULL`] only holds while `empty_old_string_does_not_override`
    /// is enabled, so it is served only for configs that opt into the guard and
    /// stripped by default (legacy overwrite behavior).
    fn versioned_definition(
        &self,
        _contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        effective_params: &serde_json::Value,
    ) -> crate::types::definition::ToolDefinition {
        let params: SearchReplaceParams =
            serde_json::from_value(effective_params.clone()).unwrap_or_default();
        let raw_desc = match description_override {
            Some(d) => d.to_string(),
            None if params.empty_old_string_does_not_override => {
                self.description_template().to_string()
            }
            None => self
                .description_template()
                .replace(EMPTY_OLD_STRING_GUARD_SENTENCE, ""),
        };
        let description = renderer.render(&raw_desc).unwrap_or_else(|e| {
            crate::types::template_renderer::strip_markers_on_render_failure(&raw_desc, &e)
        });
        let remapped_schema = if param_map.is_empty() {
            input_schema.clone()
        } else {
            crate::util::remap::remap_schema_properties(input_schema, param_map)
        };
        crate::types::definition::ToolDefinition::function(
            client_name,
            Some(&description),
            remapped_schema,
        )
    }
    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["FileWritten"]
    }
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::And(vec![
            // Unless `skip_read_before_edit` is set, require a Read tool in the toolset
            // (read-before-edit is encouraged via description and RL grading, not runtime-enforced).
            Expr::Value(ToolRequirement::if_params(
                Expr::Not(Box::new(Expr::Value(ToolParamsRequirement::new(
                    "skip_read_before_edit",
                    true,
                )))),
                ToolRequirement::tool_kind(ToolKind::Read),
            )),
            // Description template references these input params via
            // ${{ params.edit.old_string }}, ${{ params.edit.new_string }},
            // ${{ params.edit.replace_all }}. They must remain visible.
            // TODO: We can generate the schemas and requirement by enforcing
            // it during the registry phase, since these are parts of the params which are
            // tied to the tool
            Expr::Value(ToolRequirement::input_param(ToolKind::Edit, "old_string")),
            Expr::Value(ToolRequirement::input_param(ToolKind::Edit, "new_string")),
            Expr::Value(ToolRequirement::input_param(ToolKind::Edit, "replace_all")),
        ])
    }
}
impl echo_agent_tool_runtime::Tool for SearchReplaceTool {
    type Args = SearchReplaceInput;
    type Output = SearchReplaceOutput;
    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("search_replace").expect("valid tool id")
    }
    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "search_replace",
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
        name = "tool.search_replace",
        skip_all,
        fields(file_path = %input.file_path, replace_all = %input.replace_all)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: SearchReplaceInput,
    ) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let bv = crate::types::tool_metadata::behavior_version(&ctx);
        let is_legacy = SearchReplaceVersion::from_contract(bv.as_deref()).is_legacy();
        let file_path = input.file_path.clone();
        let result = run_search_replace(input, &ctx, resources.clone()).await?;
        if is_legacy {
            versions::legacy_0_4_10::downgrade_structured_errors(result, &resources, &file_path)
                .await
        } else {
            Ok(result)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::{test_ctx, test_ctx_with_call_id};
    use crate::{computer::local::LocalFs, types::resources::Resources};
    use std::sync::Arc;
    use tempfile::TempDir;
    /// Set up Resources with real filesystem for tests.
    fn test_resources(cwd: &std::path::Path) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        let edit_params = std::collections::HashMap::from([
            ("old_string".to_string(), "old_string".to_string()),
            ("new_string".to_string(), "new_string".to_string()),
            ("replace_all".to_string(), "replace_all".to_string()),
        ]);
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([
                (ToolKind::Read, "read_file".to_string()),
                (ToolKind::Execute, "run_terminal_cmd".to_string()),
            ]),
            std::collections::HashMap::from([(ToolKind::Edit, edit_params)]),
        ));
        resources
    }
    fn make_input(file_path: &str, old_string: &str, new_string: &str) -> SearchReplaceInput {
        SearchReplaceInput {
            file_path: file_path.to_string(),
            old_string: old_string.to_string(),
            new_string: new_string.to_string(),
            replace_all: false,
        }
    }
    fn description_renderer() -> TemplateRenderer {
        let edit_params = std::collections::HashMap::from([
            ("old_string".to_string(), "old_string".to_string()),
            ("new_string".to_string(), "new_string".to_string()),
            ("replace_all".to_string(), "replace_all".to_string()),
        ]);
        TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "read_file".to_string())]),
            std::collections::HashMap::from([(ToolKind::Edit, edit_params)]),
        )
    }
    /// The strip in `versioned_definition` matches the template verbatim, so
    /// the sentence must stay in sync with `DESCRIPTION_FULL`.
    #[test]
    fn overwrite_guard_sentence_stays_in_sync_with_template() {
        assert!(DESCRIPTION_FULL.contains(EMPTY_OLD_STRING_GUARD_SENTENCE));
    }
    #[test]
    fn overwrite_guard_sentence_is_conditional_on_param() {
        use crate::types::tool_metadata::ToolMetadata;
        let renderer = description_renderer();
        let schema = serde_json::json!({"type": "object", "properties": {}});
        let param_map = std::collections::HashMap::new();
        let default_def = ToolMetadata::versioned_definition(
            &SearchReplaceTool,
            None,
            "search_replace",
            None,
            &renderer,
            &param_map,
            &schema,
            &serde_json::json!({}),
        );
        let default_desc = default_def.function.description.unwrap();
        assert!(
            !default_desc.contains("cannot overwrite"),
            "guard sentence must be absent by default (legacy overwrite behavior):\n{default_desc}"
        );
        assert!(
            default_desc.contains("To create a new file"),
            "create-file guidance must remain:\n{default_desc}"
        );
        let opt_in_def = ToolMetadata::versioned_definition(
            &SearchReplaceTool,
            None,
            "search_replace",
            None,
            &renderer,
            &param_map,
            &schema,
            &serde_json::json!({"empty_old_string_does_not_override": true}),
        );
        let opt_in_desc = opt_in_def.function.description.unwrap();
        assert!(
            opt_in_desc.contains("cannot overwrite an existing non-empty file"),
            "guard sentence must appear when the guard is enabled:\n{opt_in_desc}"
        );
    }
    #[test]
    fn description_read_bullet_guarded_on_read_tool() {
        use crate::types::tool_metadata::ToolMetadata;
        let rendered = description_renderer()
            .render(ToolMetadata::description_template(&SearchReplaceTool))
            .unwrap();
        assert!(
            rendered.contains("read_file` prefixes each line") && !rendered.contains("${%"),
            "read bullet must render with the resolved name:\n{rendered}"
        );
        assert!(
            !rendered.contains("before editing it"),
            "read-before-edit guidance must be gone:\n{rendered}"
        );
        let edit_params = std::collections::HashMap::from([
            ("old_string".to_string(), "old_string".to_string()),
            ("new_string".to_string(), "new_string".to_string()),
            ("replace_all".to_string(), "replace_all".to_string()),
        ]);
        let no_read = TemplateRenderer::new(
            std::collections::HashMap::new(),
            std::collections::HashMap::from([(ToolKind::Edit, edit_params)]),
        )
        .render(ToolMetadata::description_template(&SearchReplaceTool))
        .unwrap();
        assert!(
            !no_read.contains("prefixes each line") && !no_read.contains("- \n"),
            "read bullet must vanish cleanly without a Read tool:\n{no_read}"
        );
    }
    #[tokio::test]
    async fn basic_replacement() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();
        let tool = SearchReplaceTool;
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
    #[tokio::test]
    async fn new_file_creation() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
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
    /// Harness configs still send this field; it must keep validating under `deny_unknown_fields`.
    #[test]
    fn harness_skip_read_before_edit_param_still_validates() {
        let json = serde_json::json!({ "skip_read_before_edit": true });
        crate::types::params_validation::validate_params_json::<SearchReplaceParams>(&json).expect(
            "harness skip_read_before_edit config must validate against SearchReplaceParams",
        );
    }
    /// Consecutive edits to the same file succeed without any prior read.
    #[tokio::test]
    async fn consecutive_edits_succeed_without_prior_read() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();
        let tool = SearchReplaceTool;
        let resources = test_resources(tmp.path());
        let shared = resources.into_shared();
        let result1 = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(shared.clone()),
            make_input("test.txt", "hello", "hi"),
        )
        .await
        .unwrap();
        assert!(
            matches!(result1, SearchReplaceOutput::EditsApplied(_)),
            "first edit unexpectedly returned {:?}",
            result1
        );
        let result2 = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(shared),
            make_input("test.txt", "world", "earth"),
        )
        .await
        .unwrap();
        assert!(
            matches!(result2, SearchReplaceOutput::EditsApplied(_)),
            "second edit unexpectedly returned {:?}",
            result2
        );
        let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
        assert_eq!(content, "hi earth\n");
    }
    #[tokio::test]
    async fn skip_read_before_edit_param() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "goodbye\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn file_not_found() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("nonexistent.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::FileNotFound(msg) => {
                assert!(msg.contains("does not exist"), "got: {msg}");
            }
            other => panic!("Expected FileNotFound, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn legacy_file_not_found_returns_exact_historical_invalid_input() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("nonexistent.txt", "hello", "goodbye");
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let result = echo_agent_tool_runtime::Tool::run(&tool, ctx, input)
            .await
            .unwrap();
        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert_eq!(
                    msg,
                    "File not found: nonexistent.txt. Please check the path and try again."
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn rejects_directory() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();
        let tool = SearchReplaceTool;
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
    #[tokio::test]
    async fn create_file_under_file_path() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("exception"), "not a dir\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("exception/Foo.java", "", "public class Foo {}");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert!(msg.contains("already exists as a file"), "got: {msg}");
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn file_not_found_uses_error_kind() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("nonexistent.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::FileNotFound(msg) => {
                assert!(msg.contains("does not exist"), "got: {msg}");
            }
            other => panic!("Expected FileNotFound, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn rejects_same_old_new() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
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
    async fn replace_all_mode() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa bbb aaa\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = SearchReplaceInput {
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
    #[tokio::test]
    async fn multiple_matches_without_replace_all() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "aaa", "ccc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::MultipleMatchesFound(msg) => {
                assert!(
                    msg.contains("replace_all"),
                    "Should mention replace_all: {}",
                    msg
                );
            }
            other => panic!("Expected MultipleMatchesFound, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn legacy_multiple_matches_returns_exact_historical_invalid_input() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "aaa", "ccc");
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let result = echo_agent_tool_runtime::Tool::run(&tool, ctx, input)
            .await
            .unwrap();
        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert_eq!(
                    msg,
                    "The string to replace was found multiple times in the file. Use replace_all to replace all occurrences, or include more context to only edit one occurrence."
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn no_match_found() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "xyz", "abc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::NoMatchesFound(ref e) => {
                let msg = &e.message;
                assert!(
                    msg.contains("read_file"),
                    "Should mention read_file: {}",
                    msg
                );
            }
            other => panic!("Expected NoMatchesFound, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn legacy_no_match_returns_exact_historical_invalid_input() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "xyz", "abc");
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let result = echo_agent_tool_runtime::Tool::run(&tool, ctx, input)
            .await
            .unwrap();
        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert_eq!(
                    msg,
                    "The string to replace was not found in the file, use the read_file tool to see the correct string."
                );
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn file_already_exists_nonempty() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "existing content\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: false,
            empty_old_string_does_not_override: true,
            ..Default::default()
        }));
        let input = make_input("existing.txt", "", "new content");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::FileAlreadyExists(msg) => {
                assert!(
                    msg.contains("old_string"),
                    "Should mention old_string: {}",
                    msg
                );
            }
            other => panic!("Expected FileAlreadyExists, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn empty_old_string_overwrites_existing_file_by_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "existing content\n").unwrap();
        let tool = SearchReplaceTool;
        let resources = test_resources(tmp.path());
        let input = make_input("existing.txt", "", "completely new content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(applied.tool_output_for_prompt.contains("has been created"));
                let content = std::fs::read_to_string(tmp.path().join("existing.txt")).unwrap();
                assert_eq!(content, "completely new content\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn empty_old_string_overrides_with_explicit_false() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "existing content\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("existing.txt", "", "completely new content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(applied.tool_output_for_prompt.contains("has been created"));
                let content = std::fs::read_to_string(tmp.path().join("existing.txt")).unwrap();
                assert_eq!(content, "completely new content\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn empty_old_string_blocked_when_param_set() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("existing.txt"), "existing content\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: false,
            empty_old_string_does_not_override: true,
            ..Default::default()
        }));
        let input = make_input("existing.txt", "", "replacement content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::FileAlreadyExists(msg) => {
                assert!(
                    msg.contains("old_string"),
                    "Should mention old_string: {}",
                    msg
                );
                let content = std::fs::read_to_string(tmp.path().join("existing.txt")).unwrap();
                assert_eq!(content, "existing content\n");
            }
            other => panic!("Expected FileAlreadyExists, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn empty_old_string_creates_new_file_even_with_override_guard() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: false,
            empty_old_string_does_not_override: true,
            ..Default::default()
        }));
        let input = make_input("brand_new.txt", "", "fresh content\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                assert!(applied.tool_output_for_prompt.contains("has been created"));
                let content = std::fs::read_to_string(tmp.path().join("brand_new.txt")).unwrap();
                assert_eq!(content, "fresh content\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn empty_old_string_overwrites_empty_file_even_with_guard() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("empty.txt"), "").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: false,
            empty_old_string_does_not_override: true,
            ..Default::default()
        }));
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
    #[tokio::test]
    async fn param_name_mapping_in_errors() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let mut param_map = std::collections::HashMap::new();
        let mut sr_params = std::collections::HashMap::new();
        sr_params.insert("replace_all".to_string(), "replaceAll".to_string());
        param_map.insert(ToolKind::Edit, sr_params);
        resources.insert(TemplateRenderer::new(Default::default(), param_map));
        let input = make_input("test.txt", "aaa", "ccc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::MultipleMatchesFound(msg) => {
                assert!(
                    msg.contains("replaceAll"),
                    "Should use mapped param name 'replaceAll': {}",
                    msg
                );
            }
            other => panic!("Expected MultipleMatchesFound, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn fully_randomized_names_appear_in_multiple_matches_error() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            ..Default::default()
        }));
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([
                (ToolKind::Read, "file_reader".to_string()),
                (ToolKind::Execute, "shell".to_string()),
            ]),
            std::collections::HashMap::from([(
                ToolKind::Edit,
                std::collections::HashMap::from([
                    ("old_string".to_string(), "find".to_string()),
                    ("new_string".to_string(), "replace".to_string()),
                    ("replace_all".to_string(), "replaceEverything".to_string()),
                ]),
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
    #[tokio::test]
    async fn notification_emitted_on_edit() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello\n").unwrap();
        let (handle, mut rx) = ToolNotificationHandle::channel();
        let tool = SearchReplaceTool;
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(handle));
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "hello", "goodbye");
        echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "call-99"),
            input,
        )
        .await
        .unwrap();
        let notification = rx.try_recv().unwrap();
        match notification {
            crate::notification::types::ToolNotification::FileWritten(fw) => {
                assert_eq!(fw.tool_call_id, "call-99");
                assert_eq!(fw.content, "goodbye\n");
                assert_eq!(fw.previous_content, Some("hello\n".to_string()));
                assert!(!fw.is_new_file);
            }
            other => panic!("Expected FileWritten notification, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn notification_emitted_on_create() {
        let tmp = TempDir::new().unwrap();
        let (handle, mut rx) = ToolNotificationHandle::channel();
        let tool = SearchReplaceTool;
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(handle));
        let input = make_input("new.txt", "", "brand new\n");
        echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "call-100"),
            input,
        )
        .await
        .unwrap();
        let notification = rx.try_recv().unwrap();
        match notification {
            crate::notification::types::ToolNotification::FileWritten(fw) => {
                assert_eq!(fw.tool_call_id, "call-100");
                assert_eq!(fw.content, "brand new\n");
                assert!(fw.previous_content.is_none());
                assert!(fw.is_new_file);
            }
            other => panic!("Expected FileWritten notification, got {:?}", other),
        }
    }
    fn build_gitignore(root: &std::path::Path, patterns: &[&str]) -> ignore::gitignore::Gitignore {
        let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
        for pattern in patterns {
            builder.add_line(None, pattern).unwrap();
        }
        builder.build().unwrap()
    }
    fn test_resources_with_gitignore(cwd: &std::path::Path) -> Resources {
        let mut resources = test_resources(cwd);
        let canonical = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let gi = build_gitignore(&canonical, &["build/", "dist/", "*.min.js"]);
        resources.insert(GitignoreFilter::new(gi, canonical));
        resources
    }
    #[tokio::test]
    async fn edit_blocked_by_gitignore() {
        let tmp = TempDir::new().unwrap();
        let canonical_root = dunce::canonicalize(tmp.path()).unwrap();
        let build_dir = canonical_root.join("build");
        std::fs::create_dir(&build_dir).unwrap();
        std::fs::write(build_dir.join("output.js"), "var x = 1;\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources_with_gitignore(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("build/output.js", "var x = 1;", "var x = 2;");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert!(
                    msg.contains("ignored by .gitignore"),
                    "Error should mention .gitignore: {}",
                    msg
                );
            }
            other => panic!("Expected InvalidInput for gitignored file, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn legacy_edit_allowed_for_gitignored_file() {
        let tmp = TempDir::new().unwrap();
        let canonical_root = dunce::canonicalize(tmp.path()).unwrap();
        let build_dir = canonical_root.join("build");
        std::fs::create_dir(&build_dir).unwrap();
        let file_path = build_dir.join("output.js");
        std::fs::write(&file_path, "var x = 1;\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources_with_gitignore(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("build/output.js", "var x = 1;", "var x = 2;");
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));
        let result = echo_agent_tool_runtime::Tool::run(&tool, ctx, input)
            .await
            .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read_to_string(&file_path).unwrap();
                assert!(content.contains("var x = 2;"));
            }
            other => {
                panic!(
                    "Expected EditsApplied for legacy gitignored file, got {:?}",
                    other
                )
            }
        }
    }
    #[tokio::test]
    async fn create_file_blocked_by_gitignore() {
        let tmp = TempDir::new().unwrap();
        let canonical_root = dunce::canonicalize(tmp.path()).unwrap();
        let dist_dir = canonical_root.join("dist");
        std::fs::create_dir(&dist_dir).unwrap();
        let tool = SearchReplaceTool;
        let resources = test_resources_with_gitignore(tmp.path());
        let input = make_input("dist/bundle.js", "", "console.log('hello');\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::InvalidInput(msg) => {
                assert!(msg.contains("ignored by .gitignore"));
            }
            other => {
                panic!(
                    "Expected InvalidInput for new file in gitignored dir, got {:?}",
                    other
                )
            }
        }
    }
    #[tokio::test]
    async fn edit_allowed_when_not_gitignored() {
        let tmp = TempDir::new().unwrap();
        let canonical_root = dunce::canonicalize(tmp.path()).unwrap();
        let src_dir = canonical_root.join("src");
        std::fs::create_dir(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "fn main() {}\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources_with_gitignore(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input(
            "src/main.rs",
            "fn main() {}",
            "fn main() { println!(\"hi\"); }",
        );
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read_to_string(src_dir.join("main.rs")).unwrap();
                assert!(content.contains("println"));
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    #[tokio::test]
    async fn edit_no_gitignore_filter_allows_all() {
        let tmp = TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        std::fs::create_dir(&build_dir).unwrap();
        std::fs::write(build_dir.join("output.js"), "var x = 1;\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("build/output.js", "var x = 1;", "var x = 2;");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {}
            other => {
                panic!(
                    "Expected EditsApplied when no gitignore filter, got {:?}",
                    other
                )
            }
        }
    }
    #[tokio::test]
    async fn rejects_filename_too_long() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
        let resources = test_resources(tmp.path());
        let long_name = "a".repeat(256);
        let long_path = format!("dir/{long_name}.txt");
        let input = make_input(&long_path, "", "content");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::FilenameTooLong(msg) => {
                assert!(
                    msg.contains("character limit"),
                    "Should mention the limit: {}",
                    msg
                );
                assert!(
                    msg.contains("260 characters"),
                    "Should mention the actual length: {}",
                    msg
                );
            }
            other => {
                panic!(
                    "Expected FilenameTooLong for filename too long, got {:?}",
                    other
                )
            }
        }
    }
    #[tokio::test]
    async fn accepts_filename_at_max_length() {
        let tmp = TempDir::new().unwrap();
        let tool = SearchReplaceTool;
        let resources = test_resources(tmp.path());
        let name_255 = "b".repeat(255);
        let input = make_input(&name_255, "", "content");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        if let SearchReplaceOutput::FilenameTooLong(msg) = &result {
            panic!("255-char filename should be allowed, got: {msg}");
        }
    }
    #[test]
    fn validate_path_length_catches_long_component() {
        let long_name = "x".repeat(300);
        let path = format!("src/{long_name}/file.rs");
        let result = validate_path_length(&path);
        assert!(result.is_some(), "Should reject 300-char component");
        match result.unwrap() {
            SearchReplaceOutput::FilenameTooLong(msg) => {
                assert!(msg.contains("300 characters"));
                assert!(msg.contains("character limit"));
            }
            other => panic!("Expected FilenameTooLong, got {:?}", other),
        }
    }
    #[test]
    fn validate_path_length_allows_normal_paths() {
        assert!(validate_path_length("src/main.rs").is_none());
        assert!(validate_path_length("a/b/c/d/e/f/g.txt").is_none());
        assert!(validate_path_length("/absolute/path/to/file.rs").is_none());
    }
    #[test]
    fn hint_returns_formatted_line_when_keyword_found() {
        let file = "foo bar
oCollMode_set,
    neutTest_set);";
        let old_string = "				oCollMode_set,
				neutTest_set);";
        let hint = build_nearest_match_hint(file, old_string);
        assert_eq!(
            hint,
            "

Nearest match: line 2: oCollMode_set,"
        );
    }
    #[test]
    fn hint_empty_when_keyword_not_in_file() {
        let file = "alpha beta
gamma delta";
        let old_string = "oCollMode_set,";
        let hint = build_nearest_match_hint(file, old_string);
        assert!(hint.is_empty(), "expected empty hint when keyword absent");
    }
    #[test]
    fn hint_empty_when_old_string_has_no_tokens() {
        let hint = build_nearest_match_hint(
            "some content",
            "   	  
  ",
        );
        assert!(hint.is_empty());
    }
    #[test]
    fn hint_capped_at_200_chars() {
        let long_line = format!("oCollMode{}", "x".repeat(500));
        let file = format!(
            "other line
{long_line}"
        );
        let hint = build_nearest_match_hint(&file, "oCollMode");
        assert!(
            hint.len() <= 200,
            "hint must not exceed 200 chars, got {}",
            hint.len()
        );
        assert!(hint.ends_with('…'), "truncated hint must end with ellipsis");
    }
    #[test]
    fn hint_picks_longest_token_from_first_line() {
        let file = "if ret\noCollCustomRhoMode_set, // target line\nother";
        let hint = build_nearest_match_hint(file, "oCollCustomRhoMode_set,\nneutTest_set);");
        assert!(
            hint.contains("oCollCustomRhoMode_set"),
            "should match on longest token, got: {hint}"
        );
    }
    /// Integration: NoMatchesFound message includes the nearest-match hint.
    #[tokio::test]
    async fn no_matches_message_includes_hint() {
        let tmp = TempDir::new().unwrap();
        let content = "foo bar
oCollMode_set, // line 2
neutTest_set);
";
        std::fs::write(tmp.path().join("main.c"), content).unwrap();
        let tool = SearchReplaceTool;
        let resources = test_resources(tmp.path());
        let input = make_input("main.c", "			oCollMode_set,", "replaced");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::NoMatchesFound(e) => {
                assert!(
                    e.message.contains("Nearest match: line"),
                    "message should include nearest-match hint, got: {}",
                    e.message
                );
            }
            other => panic!("Expected NoMatchesFound, got {:?}", other),
        }
    }
    /// Every script tool present — for tests that only care about the
    /// diagnostic logic, not which tools the host happens to have.
    fn test_tools() -> crate::util::query_tools::QueryTools {
        crate::util::query_tools::QueryTools {
            jq: Some("jq"),
            python: Some("python3"),
            sed: Some("sed"),
            cut: Some("cut"),
        }
    }
    /// The terminal fallback names only installed script tools (mirrors the
    /// `use_tool` MCP-dump steer; never suggest a tool that isn't there).
    #[test]
    fn confusable_hint_names_only_installed_script_tools() {
        let file = "She said \u{201C}hello\u{201D}\n";
        let tools = crate::util::query_tools::QueryTools {
            jq: None,
            python: None,
            sed: Some("sed"),
            cut: None,
        };
        let hint = build_confusable_hint(
            file,
            "\"hello\"",
            tools,
            "read_file",
            "old_string",
            "run_terminal_cmd",
        )
        .expect("should produce a confusable hint");
        assert!(hint.contains("`sed`"), "names the present sed: {hint}");
        assert!(
            !hint.contains("python"),
            "must not name absent python: {hint}"
        );
    }
    /// Template-derived names can render blank (no Execute tool, read guard
    /// disabled, missing param mapping); the hint must never emit a dangling
    /// reference to a blank name.
    #[test]
    fn confusable_hint_guards_blank_template_names() {
        let file = "She said \u{201C}hello\u{201D}\n";
        let hint = build_confusable_hint(file, "\"hello\"", test_tools(), "", "", "")
            .expect("should produce a confusable hint");
        assert!(
            !hint.contains(" in  output"),
            "no dangling read-tool reference: {hint}"
        );
        assert!(
            hint.contains("old_string"),
            "falls back to the canonical param name: {hint}"
        );
        assert!(
            !hint.contains(", or use "),
            "no terminal fallback without an Execute tool: {hint}"
        );
        assert!(
            !hint.contains("  "),
            "no double spaces from blank substitutions: {hint}"
        );
    }
    /// With no script tools installed, the terminal fallback is omitted
    /// entirely — the ASCII-anchor advice needs no external tool.
    #[test]
    fn confusable_hint_omits_terminal_fallback_when_no_script_tools() {
        let file = "She said \u{201C}hello\u{201D}\n";
        let hint = build_confusable_hint(
            file,
            "\"hello\"",
            crate::util::query_tools::QueryTools::default(),
            "read_file",
            "old_string",
            "run_terminal_cmd",
        )
        .expect("should produce a confusable hint");
        assert!(
            hint.contains("ASCII-only context"),
            "keeps the tool-free recovery advice: {hint}"
        );
        assert!(
            !hint.contains("python") && !hint.contains("sed") && !hint.contains("script"),
            "no terminal fallback when no script tools exist: {hint}"
        );
        assert!(
            !hint.contains("run_terminal_cmd"),
            "must not steer to the shell tool with nothing to run: {hint}"
        );
    }
    #[test]
    fn confusable_hint_none_for_pure_ascii_file() {
        let file = "hello world\nfoo bar\n";
        assert!(
            build_confusable_hint(
                file,
                "xyz",
                test_tools(),
                "read_file",
                "old_string",
                "run_terminal_cmd"
            )
            .is_none(),
            "no hint when file has no confusables"
        );
    }
    #[test]
    fn confusable_hint_none_when_normalized_miss_also_fails() {
        let file = "She said \u{201C}hello\u{201D}\n";
        assert!(
            build_confusable_hint(
                file,
                "totally_different_string",
                test_tools(),
                "read_file",
                "old_string",
                "run_terminal_cmd"
            )
            .is_none(),
            "no false guidance when confusables are unrelated to the miss"
        );
    }
    #[test]
    fn confusable_hint_present_when_normalized_match_would_succeed() {
        let file = "the fix should be \u{201C}stream through\u{201D}\n";
        let hint = build_confusable_hint(
            file,
            "\"stream through\"",
            test_tools(),
            "read_file",
            "old_string",
            "run_terminal_cmd",
        );
        let hint = hint.expect("should produce a confusable hint");
        assert!(
            hint.contains("Unicode typography characters"),
            "hint should mention Unicode typography: {}",
            hint
        );
        assert!(
            hint.contains("lines 1"),
            "hint should mention affected line number: {}",
            hint
        );
    }
    #[test]
    fn confusable_hint_reports_only_matched_region_lines() {
        let file = "line one\n\u{201C}line two\u{201D}\nline three\n\u{2014}line four\n";
        let hint = build_confusable_hint(
            file,
            "\"line two\"",
            test_tools(),
            "read_file",
            "old_string",
            "run_terminal_cmd",
        );
        let hint = hint.expect("should produce a confusable hint");
        assert!(hint.contains('2'), "should mention line 2: {}", hint);
        assert!(
            !hint.contains('4'),
            "should NOT mention line 4 (outside match region): {}",
            hint
        );
    }
    #[test]
    fn confusable_hint_multi_line_match_region() {
        let file = "header\n\u{201C}start\nend\u{201D}\nfooter\n";
        let hint = build_confusable_hint(
            file,
            "\"start\nend\"",
            test_tools(),
            "read_file",
            "old_string",
            "run_terminal_cmd",
        );
        let hint = hint.expect("should produce a confusable hint");
        assert!(hint.contains('2'), "should mention line 2: {}", hint);
        assert!(hint.contains('3'), "should mention line 3: {}", hint);
    }
    #[test]
    fn confusable_hint_caps_many_lines() {
        let mut file = String::new();
        for i in 1..=12 {
            file.push_str(&format!("line {}\u{00A0}content\n", i));
        }
        let mut old_string = String::new();
        for i in 1..=12 {
            old_string.push_str(&format!("line {} content\n", i));
        }
        let hint = build_confusable_hint(
            &file,
            &old_string,
            test_tools(),
            "read_file",
            "old_string",
            "run_terminal_cmd",
        );
        let hint = hint.expect("should produce a confusable hint");
        assert!(
            hint.contains("and 4 more"),
            "should cap line list with 'and N more': {}",
            hint
        );
    }
    /// Integration: NoMatchesFound message includes confusable hint for smart quotes.
    #[tokio::test]
    async fn no_matches_includes_confusable_hint_for_smart_quotes() {
        let tmp = TempDir::new().unwrap();
        let content = "the fix should be \u{201C}stream through\u{201D}\n";
        std::fs::write(tmp.path().join("doc.md"), content).unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("doc.md", "\"stream through\"", "replacement");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::NoMatchesFound(e) => {
                assert!(
                    e.message.contains("Unicode typography characters"),
                    "should include confusable guidance, got: {}",
                    e.message
                );
                assert!(
                    e.message.contains("read_file"),
                    "should still mention read_file, got: {}",
                    e.message
                );
            }
            other => panic!("Expected NoMatchesFound, got {:?}", other),
        }
    }
    /// Integration: NoMatchesFound message has NO confusable hint for plain ASCII miss.
    #[tokio::test]
    async fn no_matches_no_confusable_hint_for_ascii_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "xyz", "abc");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::NoMatchesFound(e) => {
                assert!(
                    !e.message.contains("Unicode typography"),
                    "should NOT include confusable guidance for ASCII file, got: {}",
                    e.message
                );
            }
            other => panic!("Expected NoMatchesFound, got {:?}", other),
        }
    }
    /// Integration: confusables in file but unrelated to the missed old_string
    /// should NOT produce false guidance.
    #[tokio::test]
    async fn no_matches_no_false_confusable_guidance() {
        let tmp = TempDir::new().unwrap();
        let content = "\u{201C}quoted text\u{201D}\nplain text\n";
        std::fs::write(tmp.path().join("doc.md"), content).unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("doc.md", "totally_unrelated_string", "replacement");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::NoMatchesFound(e) => {
                assert!(
                    !e.message.contains("Unicode typography"),
                    "should NOT include confusable guidance when confusables are unrelated, got: {}",
                    e.message
                );
            }
            other => panic!("Expected NoMatchesFound, got {:?}", other),
        }
    }
    fn fallback_params() -> SearchReplaceParams {
        SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            unicode_normalized_fallback: true,
            include_user_edit_hint: false,
        }
    }
    /// Exact match still works and returns unicode_normalized=false.
    #[tokio::test]
    async fn fallback_exact_match_still_preferred() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello world\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(
                    !a.unicode_normalized,
                    "exact match should not set unicode_normalized"
                );
                let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert_eq!(content, "goodbye world\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Smart quotes fallback succeeds with unicode_normalized=true.
    #[tokio::test]
    async fn fallback_smart_quotes() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("f.txt"),
            "say \u{201C}hello\u{201D} world\n",
        )
        .unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "\"hello\"", "\"goodbye\"");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(a.unicode_normalized, "should set unicode_normalized=true");
                let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert_eq!(content, "say \"goodbye\" world\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Em-dash fallback succeeds.
    #[tokio::test]
    async fn fallback_em_dash() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "foo\u{2014}bar\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "foo--bar", "foo-bar");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(a.unicode_normalized);
                let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert_eq!(content, "foo-bar\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// NBSP fallback succeeds.
    #[tokio::test]
    async fn fallback_nbsp() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "hello\u{00A0}world\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "hello world", "hello_world");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(a.unicode_normalized);
                let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert_eq!(content, "hello_world\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Ellipsis fallback succeeds.
    #[tokio::test]
    async fn fallback_ellipsis() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "wait\u{2026}\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "wait...", "done");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(a.unicode_normalized);
                let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert_eq!(content, "done\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Multi-match + replace_all=false returns MultipleMatchesFound.
    #[tokio::test]
    async fn fallback_multi_match_without_replace_all() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("f.txt"),
            "\u{201C}a\u{201D} and \u{201C}a\u{201D}\n",
        )
        .unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "\"a\"", "\"b\"");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::MultipleMatchesFound(msg) => {
                assert!(
                    msg.contains("multiple times") || msg.contains("Unicode normalization"),
                    "should mention multiple matches: {}",
                    msg
                );
            }
            other => panic!("Expected MultipleMatchesFound, got {:?}", other),
        }
    }
    /// Multi-match + replace_all=true replaces all occurrences.
    #[tokio::test]
    async fn fallback_multi_match_with_replace_all() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("f.txt"),
            "\u{201C}a\u{201D} and \u{201C}a\u{201D}\n",
        )
        .unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let mut input = make_input("f.txt", "\"a\"", "\"b\"");
        input.replace_all = true;
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(a.unicode_normalized);
                let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert_eq!(content, "\"b\" and \"b\"\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Fallback disabled by default — smart quotes produce NoMatchesFound.
    #[tokio::test]
    async fn fallback_disabled_by_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "\u{201C}hello\u{201D}\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            unicode_normalized_fallback: false,
            include_user_edit_hint: false,
        }));
        let input = make_input("f.txt", "\"hello\"", "replaced");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        assert!(
            matches!(result, SearchReplaceOutput::NoMatchesFound(_)),
            "fallback disabled should produce NoMatchesFound, got {:?}",
            result
        );
    }
    /// Exact match exists → exact path wins even when confusables present elsewhere.
    #[tokio::test]
    async fn fallback_exact_match_wins_over_normalized() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("f.txt"),
            "\"hello\" and \u{201C}hello\u{201D}\n",
        )
        .unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "\"hello\"", "\"goodbye\"");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(
                    !a.unicode_normalized,
                    "exact match should take precedence, unicode_normalized should be false"
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Replacement preserves valid UTF-8 and only mutates matched region.
    #[tokio::test]
    async fn fallback_preserves_surrounding_content() {
        let tmp = TempDir::new().unwrap();
        let content = "before \u{201C}target\u{201D} after 🎉\n";
        std::fs::write(tmp.path().join("f.txt"), content).unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(fallback_params()));
        let input = make_input("f.txt", "\"target\"", "\"replaced\"");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(a) => {
                assert!(a.unicode_normalized);
                let written = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
                assert!(
                    written.starts_with("before "),
                    "prefix preserved: {}",
                    written
                );
                assert!(
                    written.contains("\"replaced\""),
                    "replacement applied: {}",
                    written
                );
                assert!(
                    written.contains("after 🎉"),
                    "suffix + emoji preserved: {}",
                    written
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Files with CRLF (\r\n) line endings should match against LF-only
    /// old_string (since read_file strips \r), and CRLF should be preserved
    /// after the edit.
    #[tokio::test]
    async fn crlf_multiline_match_preserves_line_endings() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), b"hello\r\nworld\r\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "hello\nworld\n", "goodbye\nearth\n");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, b"goodbye\r\nearth\r\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Single-line match within a CRLF file works and preserves CRLF.
    #[tokio::test]
    async fn crlf_single_line_match_preserves_line_endings() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), b"aaa\r\nbbb\r\nccc\r\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "bbb", "BBB");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, b"aaa\r\nBBB\r\nccc\r\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// LF-only files are unaffected by CRLF normalization logic.
    #[tokio::test]
    async fn lf_only_file_unaffected_by_crlf_logic() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello\nworld\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, b"goodbye\nworld\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Replace-all mode works correctly with CRLF files.
    #[tokio::test]
    async fn crlf_replace_all() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), b"foo\r\nbar\r\nfoo\r\nbaz\r\n").unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = SearchReplaceInput {
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
            SearchReplaceOutput::EditsApplied(_) => {
                let content = std::fs::read(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, b"qux\r\nbar\r\nqux\r\nbaz\r\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
    /// Mixed line endings (\r\n and \n in the same file): CRLF normalization
    /// kicks in because the file contains at least one \r\n, so all \n in the
    /// result are converted to \r\n. This normalizes the file to consistent
    /// CRLF endings, which is the expected behavior.
    #[tokio::test]
    async fn crlf_mixed_line_endings() {
        let tmp = TempDir::new().unwrap();
        let content = b"line1\r\nline2\nline3\r\nline4\n";
        std::fs::write(tmp.path().join("test.txt"), content).unwrap();
        let tool = SearchReplaceTool;
        let mut resources = test_resources(tmp.path());
        resources.insert(Params(SearchReplaceParams {
            skip_read_before_edit: true,
            empty_old_string_does_not_override: false,
            ..Default::default()
        }));
        let input = make_input("test.txt", "line2\nline3", "REPLACED");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            SearchReplaceOutput::EditsApplied(_) => {
                let written = std::fs::read(tmp.path().join("test.txt")).unwrap();
                assert_eq!(written, b"line1\r\nREPLACED\r\nline4\r\n");
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
}
