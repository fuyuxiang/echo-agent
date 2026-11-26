//! User-message construction concern for `SessionActor`: templated prefix
//! building, rules partitioning, large-prompt offload/truncation, and image
//! payload preparation.
#![allow(clippy::items_after_test_module)]
use super::*;
/// Normalize a free-form name (e.g. an MCP server identifier) into a
/// single safe filesystem segment.
///
/// Replaces anything outside `[A-Za-z0-9._-]` with `_` so the result is a
/// portable directory name on macOS/Linux.
/// Whether `url` is an `http://` or `https://` URL — i.e. a remote URL the
/// upstream API can fetch directly. `file://` and other local schemes are
/// rejected by the API and must be inlined as a `data:` URL instead.
pub(super) fn is_remote_image_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}
/// Pick the URL value sent to the upstream API for a user-attached image.
///
/// The remote API accepts a base64 `data:` URL or an HTTP(S) URL only;
/// `file://` and other local schemes return 400. Inline bytes win when
/// present (the canonical payload); `uri` is forwarded directly only
/// when it is a remote URL with no inline bytes available.
///
/// Extracted so production and the regression tests assert against the
/// same selector — a future change to the production rule cannot drift
/// past the tests.
pub(super) fn pick_user_image_url(image: &agent_client_protocol::ImageContent) -> String {
    if let Some(uri) = image.uri.as_deref()
        && image.data.is_empty()
        && is_remote_image_url(uri)
    {
        uri.to_owned()
    } else {
        format!("data:{};base64,{}", image.mime_type, image.data)
    }
}
fn partition_rules_by_scope(
    files: Vec<echo_agent_core::prompt::agents_md::AgentConfigFile>,
    echo_agent_home: &std::path::Path,
    vendor_homes: &[(std::path::PathBuf, bool)],
    workspace_roots: &[&std::path::Path],
) -> (
    Vec<echo_agent_core::prompt::user_message::RuleEntry>,
    Vec<echo_agent_core::prompt::user_message::RuleEntry>,
) {
    let mut workspace = Vec::new();
    let mut user = Vec::new();
    for file in files {
        let is_user_rule = crate::util::is_user_instruction_path(
            std::path::Path::new(&file.file_path),
            echo_agent_home,
            vendor_homes,
            workspace_roots,
        );
        let entry = echo_agent_core::prompt::user_message::RuleEntry::from(file);
        if is_user_rule {
            user.push(entry);
        } else {
            workspace.push(entry);
        }
    }
    (workspace, user)
}
#[cfg(test)]
mod partition_rules_by_scope_tests {
    use super::partition_rules_by_scope;
    use echo_agent_core::prompt::agents_md::AgentConfigFile;
    use std::path::Path;
    fn file(path: &str) -> AgentConfigFile {
        AgentConfigFile {
            file_name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            file_path: path.to_string(),
            content: path.to_string(),
        }
    }
    fn paths(entries: &[echo_agent_core::prompt::user_message::RuleEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.content.as_str()).collect()
    }
    #[test]
    fn partitions_custom_echo_agent_and_vendor_home_rules_as_user_scope() {
        let files = vec![
            file("/custom/config/rules/a.md"),
            file("/home/user/.cursor/rules/b.md"),
            file("/repo/.cursor/rules/c.md"),
            file("/repo/src/AGENTS.md"),
            file("/custom/config/rules/d.md"),
        ];
        let vendor_homes = vec![
            (Path::new("/home/user/.claude").to_path_buf(), true),
            (Path::new("/home/user/.cursor").to_path_buf(), true),
        ];
        let (workspace, user) = partition_rules_by_scope(
            files,
            Path::new("/custom/config"),
            &vendor_homes,
            &[Path::new("/repo")],
        );
        assert_eq!(
            paths(&user),
            vec![
                "/custom/config/rules/a.md",
                "/home/user/.cursor/rules/b.md",
                "/custom/config/rules/d.md",
            ]
        );
        assert_eq!(
            paths(&workspace),
            vec!["/repo/.cursor/rules/c.md", "/repo/src/AGENTS.md"]
        );
    }
    #[test]
    fn echo_agent_home_nested_in_workspace_keeps_direct_surfaces_user_scoped() {
        let files = vec![
            file("/repo/config/AGENTS.md"),
            file("/repo/config/rules/global.md"),
            file("/repo/config/.echo-agent/rules/project.md"),
            file("/repo/config/src/AGENTS.md"),
        ];
        let (workspace, user) =
            partition_rules_by_scope(files, Path::new("/repo/config"), &[], &[Path::new("/repo")]);
        assert_eq!(
            paths(&user),
            vec!["/repo/config/AGENTS.md", "/repo/config/rules/global.md"]
        );
        assert_eq!(
            paths(&workspace),
            vec![
                "/repo/config/.echo-agent/rules/project.md",
                "/repo/config/src/AGENTS.md",
            ]
        );
    }
    #[test]
    fn vendor_home_nested_in_workspace_keeps_direct_surfaces_user_scoped() {
        let files = vec![
            file("/repo/.claude/rules/global.md"),
            file("/repo/.claude/CLAUDE.md"),
            file("/repo/.claude/.claude/rules/project.md"),
            file("/repo/.claude/src/AGENTS.md"),
        ];
        let vendor_homes = vec![(Path::new("/repo/.claude").to_path_buf(), true)];
        let (workspace, user) = partition_rules_by_scope(
            files,
            Path::new("/other/echoagent"),
            &vendor_homes,
            &[Path::new("/repo")],
        );
        assert_eq!(
            paths(&user),
            vec!["/repo/.claude/rules/global.md", "/repo/.claude/CLAUDE.md"]
        );
        assert_eq!(
            paths(&workspace),
            vec![
                "/repo/.claude/.claude/rules/project.md",
                "/repo/.claude/src/AGENTS.md",
            ]
        );
    }
    #[test]
    fn nested_echo_agent_home_workspace_files_stay_workspace_scoped() {
        let files = vec![
            file("/custom/echoagent/rules/global.md"),
            file("/custom/echoagent/worktrees/repo/.cursor/rules/project.md"),
            file("/custom/echoagent/worktrees/repo/src/AGENTS.md"),
        ];
        let (workspace, user) = partition_rules_by_scope(
            files,
            Path::new("/custom/echoagent"),
            &[],
            &[Path::new("/custom/echoagent/worktrees/repo")],
        );
        assert_eq!(paths(&user), vec!["/custom/echoagent/rules/global.md"]);
        assert_eq!(
            paths(&workspace),
            vec![
                "/custom/echoagent/worktrees/repo/.cursor/rules/project.md",
                "/custom/echoagent/worktrees/repo/src/AGENTS.md",
            ]
        );
    }
    #[test]
    fn partitioned_snapshot_bodies_match_rules_and_old_reminder() {
        let files = vec![
            AgentConfigFile {
                file_name: "AGENTS.md".into(),
                file_path: "/repo/AGENTS.md".into(),
                content: "repo-agents-body".into(),
            },
            AgentConfigFile {
                file_name: "CLAUDE.md".into(),
                file_path: "/repo/CLAUDE.md".into(),
                content: "repo-claude-body".into(),
            },
            AgentConfigFile {
                file_name: "AGENTS.md".into(),
                file_path: "/home/user/.echo-agent/AGENTS.md".into(),
                content: "home-echo-agent-body".into(),
            },
            AgentConfigFile {
                file_name: "CLAUDE.md".into(),
                file_path: "/home/user/.claude/CLAUDE.md".into(),
                content: "home-claude-body".into(),
            },
            AgentConfigFile {
                file_name: "x.md".into(),
                file_path: "/repo/.echo-agent/rules/x.md".into(),
                content: "repo-echo-agent-rules-x".into(),
            },
        ];
        let vendor_homes = vec![(Path::new("/home/user/.claude").to_path_buf(), true)];
        let (workspace, user) = partition_rules_by_scope(
            files.clone(),
            Path::new("/home/user/.echo-agent"),
            &vendor_homes,
            &[Path::new("/repo")],
        );
        let rules =
            echo_agent_core::prompt::user_message::format_rules_section(&workspace, &user).unwrap();
        let reminder =
            echo_agent_core::prompt::agents_md::format_agents_md_section(&files).unwrap();
        for body in [
            "repo-agents-body",
            "repo-claude-body",
            "home-echo-agent-body",
            "home-claude-body",
            "repo-echo-agent-rules-x",
        ] {
            assert!(rules.contains(body), "rules missing {body}");
            assert!(reminder.contains(body), "reminder missing {body}");
        }
        assert!(rules.contains("name=\"/repo/AGENTS.md\""));
        assert!(rules.contains("name=\"/repo/CLAUDE.md\""));
        assert!(rules.contains("name=\"/repo/.echo-agent/rules/x.md\""));
        assert!(rules.contains("<user_rule>\nhome-echo-agent-body\n</user_rule>"));
        assert!(rules.contains("<user_rule>\nhome-claude-body\n</user_rule>"));
        assert!(!rules.contains("## From:"));
        assert!(!rules.contains("<system-reminder>"));
    }
    #[test]
    fn fork_ondisk_and_display_prefixes_both_count_as_workspace() {
        let files = vec![
            file("/home/user/.echo-agent/worktrees/repo/AGENTS.md"),
            file("/home/user/repo/crates/foo/AGENTS.md"),
            file("/home/user/.echo-agent/AGENTS.md"),
        ];
        let (workspace, user) = partition_rules_by_scope(
            files,
            Path::new("/home/user/.echo-agent"),
            &[],
            &[
                Path::new("/home/user/.echo-agent/worktrees/repo"),
                Path::new("/home/user/repo/crates/foo"),
            ],
        );
        assert_eq!(
            paths(&workspace),
            vec![
                "/home/user/.echo-agent/worktrees/repo/AGENTS.md",
                "/home/user/repo/crates/foo/AGENTS.md",
            ]
        );
        assert_eq!(paths(&user), vec!["/home/user/.echo-agent/AGENTS.md"]);
    }
}
/// True iff `conversation` already contains a project-instructions reminder,
/// either tagged [`SyntheticReason::ProjectInstructions`] or a legacy untagged
/// copy whose first text part starts with [`LEGACY_AGENTS_MD_REMINDER_PREFIX`].
/// Read-only; used by `spawn_session_actor` for idempotent AGENTS.md injection
/// so resumed sessions and forks don't duplicate the message.
pub(super) fn conversation_has_project_instructions(conversation: &[ConversationItem]) -> bool {
    conversation.iter().any(is_project_instructions)
}
/// A project-instructions (AGENTS.md) reminder: a `User` item tagged
/// [`SyntheticReason::ProjectInstructions`], or a legacy untagged copy whose first
/// text part starts with [`LEGACY_AGENTS_MD_REMINDER_PREFIX`]. Single source of
/// truth for both spawn-time idempotent injection and the compaction de-dup.
pub(super) fn is_project_instructions(item: &ConversationItem) -> bool {
    let ConversationItem::User(u) = item else {
        return false;
    };
    if u.synthetic_reason == Some(SyntheticReason::ProjectInstructions) {
        return true;
    }
    u.content
        .first()
        .and_then(|p| match p {
            ContentPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .is_some_and(|t| t.starts_with(LEGACY_AGENTS_MD_REMINDER_PREFIX))
}
/// Subagent spawns (incl. `resume_from`) overwrite the leading System with the fresh
/// prompt. Top-level resumed sessions keep theirs unless they still carry the legacy
/// upstream identity. Absent → insert + grow prefix.
pub(super) fn install_system_prompt(
    conversation: &mut Vec<ConversationItem>,
    inherited_prefix_len: &mut Option<usize>,
    is_subagent_spawn: bool,
    preserve_inherited_system: bool,
    system_prompt: &str,
) {
    if let Some(ConversationItem::System(sys)) = conversation.first_mut() {
        let has_legacy_upstream_identity = sys.content.contains("released by EchoAgent");
        if !preserve_inherited_system && (is_subagent_spawn || has_legacy_upstream_identity) {
            sys.content = std::sync::Arc::<str>::from(system_prompt);
        }
    } else {
        conversation.insert(0, ConversationItem::system(system_prompt.to_string()));
        if let Some(len) = inherited_prefix_len {
            *len += 1;
        }
    }
}
#[cfg(test)]
mod install_system_prompt_tests {
    use super::install_system_prompt;
    use echo_agent_sampling_types::conversation::ConversationItem;
    fn system_text(item: &ConversationItem) -> &str {
        match item {
            ConversationItem::System(s) => s.content.as_ref(),
            _ => panic!("first item is not System"),
        }
    }
    #[test]
    fn subagent_spawn_overwrites_leading_system() {
        let mut conv = vec![
            ConversationItem::system("old"),
            ConversationItem::user("hi"),
        ];
        let mut prefix = Some(1);
        install_system_prompt(&mut conv, &mut prefix, true, false, "fresh");
        assert_eq!(system_text(&conv[0]), "fresh");
        assert_eq!(prefix, Some(1), "prefix unchanged — System already present");
    }
    #[test]
    fn preserve_inherited_system_keeps_head_untouched() {
        let mut conv = vec![
            ConversationItem::system("parent system verbatim"),
            ConversationItem::user("hi"),
        ];
        let mut prefix = Some(2);
        install_system_prompt(&mut conv, &mut prefix, true, true, "child fresh prompt");
        assert_eq!(
            system_text(&conv[0]),
            "parent system verbatim",
            "preserve_inherited_system must not overwrite the inherited head"
        );
        assert_eq!(prefix, Some(2), "prefix unchanged — System already present");
    }
    #[test]
    fn top_level_resume_replaces_legacy_upstream_identity() {
        let mut conv = vec![
            ConversationItem::system("You are EchoAgent released by EchoAgent."),
            ConversationItem::user("hi"),
        ];
        let mut prefix = None;
        install_system_prompt(&mut conv, &mut prefix, false, false, "fresh Echo prompt");
        assert_eq!(system_text(&conv[0]), "fresh Echo prompt");
    }
    #[test]
    fn summarized_fork_system_placeholder_is_overwritten() {
        let mut conv = vec![
            ConversationItem::system("parent system placeholder"),
            ConversationItem::user("<background_context>…</background_context>"),
        ];
        let mut prefix = Some(2);
        install_system_prompt(&mut conv, &mut prefix, true, false, "child subagent system");
        assert_eq!(
            system_text(&conv[0]),
            "child subagent system",
            "summarized fork (preserve=false) must overwrite [0] with the child system"
        );
    }
    #[test]
    fn top_level_resume_keeps_stored_system() {
        let mut conv = vec![
            ConversationItem::system("stored"),
            ConversationItem::user("hi"),
        ];
        let mut prefix = None;
        install_system_prompt(&mut conv, &mut prefix, false, false, "fresh");
        assert_eq!(system_text(&conv[0]), "stored");
    }
    #[test]
    fn inserts_system_and_bumps_prefix_when_absent() {
        let mut conv = vec![ConversationItem::user("hi")];
        let mut prefix = Some(0);
        install_system_prompt(&mut conv, &mut prefix, true, false, "fresh");
        assert_eq!(system_text(&conv[0]), "fresh");
        assert_eq!(
            prefix,
            Some(1),
            "inserted System grows the preserved prefix"
        );
    }
}
pub(super) const LARGE_PROMPT_THRESHOLD: usize = 25_000;
pub(super) const TRUNCATED_PROMPT_PREFIX_SIZE: usize = 25_000;
/// Percent of the bounded-prompt budget given to the query (capped; rest is context head).
const LARGE_QUERY_BUDGET_PERCENT: usize = 80;
/// Bytes kept at the TAIL when bounding head+tail, so a trailing question survives.
const BOUNDED_TAIL_BUDGET: usize = 4_000;
/// Bytes reserved for skill instructions (own budget, not crowded out by the query).
pub(super) const SKILL_INLINE_BUDGET: usize = 4_000;
/// Marker between the head and tail of an elided block. Single source of truth.
pub(super) const ELISION_MARKER: &str =
    "\n\n…[middle truncated — full text in the offloaded file]…\n\n";
/// Stable marker opening the offload notice. Single source of truth (for a future strip-on-re-read).
pub(super) const OFFLOAD_NOTICE_MARKER: &str = "[Full request offloaded to file]";
/// In-band notice that REPLACES the offload notice when the full request could
/// not be persisted to the session file (write error or task-join failure).
/// References no path — there is no file to read — so the model is never told to
/// `read_file` a file that does not exist. The bounded head+tail excerpt remains.
const OFFLOAD_FAILED_NOTICE: &str = "\n\n[Full request could not be saved to a file — the excerpt above is truncated. Answer from it, and ask the user to resend the full content if anything essential is missing.]";
/// UTF-8-safe suffix: the last `<= max_bytes` bytes of `s`, on a char boundary.
pub(super) fn truncate_bytes_suffix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}
/// Bound `s` to `budget` as HEAD + [`ELISION_MARKER`] + TAIL (trailing question survives). UTF-8-safe.
pub(super) fn bound_head_tail(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    if budget <= ELISION_MARKER.len() {
        return truncate_bytes(s, budget).to_string();
    }
    let content_budget = budget - ELISION_MARKER.len();
    let tail_len = BOUNDED_TAIL_BUDGET.min(content_budget / 2);
    let head_len = content_budget - tail_len;
    let head = truncate_bytes(s, head_len);
    let tail = truncate_bytes_suffix(s, tail_len);
    format!("{head}{ELISION_MARKER}{tail}")
}
/// Build the offload notice: marker + path to the file with the user's full request.
pub(super) fn build_offload_notice(full_message_len: usize, file_path: &std::path::Path) -> String {
    format!(
        "\n\n{OFFLOAD_NOTICE_MARKER} The text above was truncated ({full_message_len} bytes total). \
The user's FULL request — which may include their actual question and any skill instructions not shown above — is in this file:\n{}\n\
Read this file with read_file before responding; the question you must answer may only be there.",
        file_path.display(),
    )
}
/// Build the bounded in-band message for an oversized prompt already written to
/// `file_path`. Pure; preserves message ordering, stays within budget.
pub(super) fn build_truncated_prompt_message(
    context: &str,
    query: &str,
    skill_information: &str,
    is_cursor: bool,
    file_path: &std::path::Path,
    full_message_len: usize,
) -> String {
    let notice = build_offload_notice(full_message_len, file_path);
    debug_assert!(
        notice.len() < TRUNCATED_PROMPT_PREFIX_SIZE,
        "offload notice must be far smaller than the budget"
    );
    let available = TRUNCATED_PROMPT_PREFIX_SIZE.saturating_sub(notice.len());
    let skill_inline = bound_head_tail(skill_information, SKILL_INLINE_BUDGET.min(available));
    let skill_overhead = if skill_inline.is_empty() {
        0
    } else {
        1 + skill_inline.len()
    };
    let rest = available.saturating_sub(skill_overhead);
    let query_budget = rest.saturating_mul(LARGE_QUERY_BUDGET_PERCENT) / 100;
    let query_inline = bound_head_tail(query, query_budget);
    let context_budget = rest.saturating_sub(query_inline.len()).saturating_sub(2);
    let context_inline = truncate_bytes(context, context_budget);
    let query_block = if skill_inline.is_empty() {
        query_inline
    } else {
        format!("{query_inline}\n{skill_inline}")
    };
    if is_cursor {
        format!("{context_inline}{notice}\n\n{query_block}")
    } else if context_inline.is_empty() {
        format!("{query_block}{notice}")
    } else {
        format!("{query_block}\n\n{context_inline}{notice}")
    }
}
/// Replace the file-referencing offload `notice` embedded in `message` with the
/// no-file [`OFFLOAD_FAILED_NOTICE`]. Position-independent (the notice sits at the
/// end for echoagent ordering), so a failed offload never
/// leaves the model chasing a "read this file" pointer to a file that does not
/// exist. Returns `message` unchanged if the notice is absent (defensive).
pub(super) fn strip_offload_notice(message: &str, notice: &str) -> String {
    message.replacen(notice, OFFLOAD_FAILED_NOTICE, 1)
}
/// Write `full_message` via `writer`; return the bounded in-band `message` plus
/// the file path when the write succeeds. On write failure the bounded message is
/// still returned (never the oversized original, so a failed offload can't
/// reintroduce the context-window overflow) but with the file-referencing notice
/// swapped for [`OFFLOAD_FAILED_NOTICE`], so the model isn't told to read a file
/// that was never written. The injected `writer` makes this hermetically testable.
pub(super) fn write_offload_and_build(
    full_message: &str,
    message: String,
    file_path: std::path::PathBuf,
    writer: impl FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
) -> (String, Option<std::path::PathBuf>) {
    match writer(&file_path, full_message.as_bytes()) {
        Ok(()) => (message, Some(file_path)),
        Err(e) => {
            tracing::warn!(
                ?e,
                full_bytes = full_message.len(),
                "failed to write large-prompt offload file; sending bounded preview with no file reference"
            );
            let notice = build_offload_notice(full_message.len(), &file_path);
            (strip_offload_notice(&message, &notice), None)
        }
    }
}
impl SessionActor {
    /// Rewrite the user-message prefix at conversation index 1.
    /// Caller must guarantee zero turns. When `drop_startup_skill_reminder`
    /// is true, also strips the synthetic `<system-reminder>` user item.
    pub(super) fn rewrite_zero_turn_prefix(
        conversation: &mut Vec<ConversationItem>,
        new_prefix: String,
        drop_startup_skill_reminder: bool,
    ) {
        let is_prefix_slot = matches!(
            conversation.get(1),
            Some(ConversationItem::User(u)) if u.synthetic_reason.is_none()
        );
        if is_prefix_slot {
            conversation[1] = ConversationItem::user(new_prefix);
        } else {
            let insert_at = conversation.len().min(1);
            conversation.insert(insert_at, ConversationItem::user(new_prefix));
        }
        if drop_startup_skill_reminder {
            conversation.retain(|item| {
                !matches!(
                    item,
                    ConversationItem::User(u)
                        if u.synthetic_reason
                            == Some(echo_agent_sampling_types::SyntheticReason::SystemReminder)
                )
            });
        }
    }
    pub(super) async fn build_user_message_prefix(&self) -> String {
        let display_path = self
            .display_cwd
            .get()
            .map(|s| s.as_str())
            .unwrap_or(&self.session_info.cwd);
        let cwd = std::path::Path::new(display_path);
        use echo_agent_core::prompt::user_message::UserMessageTemplate;
        let (template, include_verification) = {
            let agent = self.agent.borrow();
            let def = agent.definition();
            (
                def.user_message_template.clone(),
                def.include_browser_verification(),
            )
        };
        let mut prefix_carries_fallback_date = false;
        let mut out = if !matches!(template, UserMessageTemplate::Default) {
            if let Some(rendered) = self
                .build_templated_user_message(cwd, template.clone())
                .await
            {
                rendered
            } else {
                tracing::warn!(
                    "templated user message render failed; falling back to legacy prefix"
                );
                prefix_carries_fallback_date = !template.surfaces_local_date();
                if self.startup_hints.skip_git_status {
                    construct_user_message_minimal(cwd, None)
                } else {
                    construct_user_message(cwd, self.vcs_kind, None, None).await
                }
            }
        } else if self.startup_hints.skip_git_status {
            construct_user_message_minimal(cwd, None)
        } else {
            construct_user_message(cwd, self.vcs_kind, None, None).await
        };
        if matches!(template, UserMessageTemplate::Default) && include_verification {
            let (workspace_rules, mut user_rules) = self.gather_partitioned_rules();
            user_rules.splice(
                0..0,
                echo_agent_core::prompt::browser_verification::synthetic_user_rules(),
            );
            echo_agent_core::prompt::user_message::append_rules_section(
                &mut out,
                &workspace_rules,
                &user_rules,
            );
        }
        self.last_announced_local_date
            .set(chrono::Local::now().date_naive());
        self.prefix_carries_fallback_date
            .set(prefix_carries_fallback_date);
        out
    }
    fn gather_partitioned_rules(
        &self,
    ) -> (
        Vec<echo_agent_core::prompt::user_message::RuleEntry>,
        Vec<echo_agent_core::prompt::user_message::RuleEntry>,
    ) {
        let files = self.agent.borrow().prompt_context().agents_md_files.clone();
        let echo_agent_home = echo_agent_config::echo_agent_home();
        let vendor_homes = echo_agent_config::discovery_home_dir()
            .map(|home_dir| {
                vec![
                    (
                        home_dir.join(".claude"),
                        self.rebuild_spec.compat.claude.agents,
                    ),
                    (
                        home_dir.join(".cursor"),
                        self.rebuild_spec.compat.cursor.agents,
                    ),
                ]
            })
            .unwrap_or_default();
        let on_disk_cwd = std::path::Path::new(&self.session_info.cwd);
        let on_disk_root = git2::Repository::discover(on_disk_cwd)
            .ok()
            .and_then(|repo| repo.workdir().map(std::path::Path::to_path_buf))
            .unwrap_or_else(|| on_disk_cwd.to_path_buf());
        let display_root = self
            .display_cwd
            .get()
            .map(|s| std::path::PathBuf::from(s.as_str()));
        let workspace_roots: Vec<&std::path::Path> = display_root
            .as_deref()
            .into_iter()
            .chain(std::iter::once(on_disk_root.as_path()))
            .collect();
        partition_rules_by_scope(files, &echo_agent_home, &vendor_homes, &workspace_roots)
    }
    /// Build the custom-templated first user message.
    ///
    /// Gathers session-scoped inputs (today's date, VCS status, AGENTS.md
    /// rules, skill registry, MCP servers) and dispatches through
    /// `UserMessageContext::render`.
    async fn build_templated_user_message(
        &self,
        cwd: &std::path::Path,
        template: echo_agent_core::prompt::user_message::UserMessageTemplate,
    ) -> Option<String> {
        use echo_agent_core::prompt::user_message::UserMessageContext;
        self.wait_for_mcp_templated_prefix_ready(&template).await;
        let bridge = self.agent.borrow().tool_bridge().clone();
        let (vcs_root, vcs_status) = self.gather_vcs_for_prefix(cwd).await;
        let (workspace_rules, user_rules) = self.gather_partitioned_rules();
        let mut user_rules = user_rules;
        let skills = self.slash_skills_for_resolve().await;
        let mcp_servers = self.gather_mcp_servers(cwd).await;
        if self
            .agent
            .borrow()
            .definition()
            .include_browser_verification()
        {
            user_rules.splice(
                0..0,
                echo_agent_core::prompt::browser_verification::synthetic_user_rules(),
            );
        }
        let shell = resolve_session_shell();
        let today_local = chrono::Local::now().date_naive();
        let mcps_root = Self::workspace_mcps_root(cwd).map(|p| p.to_string_lossy().to_string());
        #[allow(unused_variables)]
        let is_cursor_template = crate::session::is_cursor_user_template(&template);
        let terminals_folder = None;
        let skill_listing_budget_chars = None;
        let ctx = UserMessageContext {
            template: template.clone(),
            workspace_path: cwd.to_path_buf(),
            os_family: crate::util::uname::os_kernel_and_release(),
            shell,
            vcs_root,
            vcs_status,
            today_local: Some(today_local),
            terminals_folder,
            workspace_rules,
            user_rules,
            skills,
            skill_listing_budget_chars,
            mcp_servers,
            mcps_root,
            read_tool_name: bridge
                .render_prompt(
                    "${{ tools.by_kind.read }}",
                    &serde_json::Value::Object(Default::default()),
                )
                .await
                .unwrap_or_else(|| "Read".to_string()),
        };
        ctx.render(&bridge).await
    }
    /// Gather VCS root + status with the same 2s timeout used by the legacy
    /// `construct_user_message` path. Returns `(root, status)` -- either may
    /// be `None` if VCS is absent or the lookup timed out.
    async fn gather_vcs_for_prefix(
        &self,
        cwd: &std::path::Path,
    ) -> (Option<std::path::PathBuf>, Option<String>) {
        use echo_agent_workspace::file_system::{git_status_short, jj_status};
        use echo_agent_workspace::session::git::VcsKind;
        if matches!(self.vcs_kind, VcsKind::None) {
            return (None, None);
        }
        let root = git2::Repository::discover(cwd).ok().and_then(|repo| {
            repo.workdir().map(|p| {
                let s = p.to_string_lossy();
                let trimmed = s.trim_end_matches('/');
                std::path::PathBuf::from(trimmed)
            })
        });
        let timeout = std::time::Duration::from_secs(5);
        let status = if self.vcs_kind.is_jj() {
            tokio::time::timeout(timeout, jj_status(cwd)).await
        } else {
            tokio::time::timeout(timeout, git_status_short(cwd)).await
        };
        let status = match status {
            Ok(Ok(s)) if !s.trim().is_empty() => Some(s.trim_end().to_string()),
            _ => None,
        };
        (root, status)
    }
    /// `None` twin: descriptor materialization is unavailable in this build.
    fn workspace_mcps_root(_cwd: &std::path::Path) -> Option<std::path::PathBuf> {
        None
    }
    /// Snapshot connected MCP servers (alphabetical) with their server
    /// instructions and per-server descriptor folder paths.
    ///
    /// Side-effect: materializes per-tool / per-resource JSON descriptor
    /// files under `<mcps_root>/<sanitized_server_name>/{tools,resources}/`
    /// for any server that exposes them. Models read these
    /// before issuing `CallMcpTool` / `FetchMcpResource` calls. Errors
    /// during materialization are logged and tolerated -- the user message
    /// is still rendered with the server entry, and the model will see an
    /// empty descriptor directory rather than a missing one. No-op when the
    /// descriptor root is unavailable (`workspace_mcps_root` is `None`).
    async fn gather_mcp_servers(
        &self,
        workspace: &std::path::Path,
    ) -> Vec<echo_agent_core::prompt::user_message::McpServerEntry> {
        use echo_agent_core::prompt::user_message::McpServerEntry;
        let mcps_root = Self::workspace_mcps_root(workspace);
        let clients: Vec<(
            String,
            std::sync::Arc<crate::session::mcp_servers::McpClient>,
        )> = {
            let state = self.mcp_state.lock().await;
            tracing::debug!(
                session_id = %self.session_info.id.0,
                client_count = state.owned_clients.len() + state.shared_clients.len(),
                initializing_count = state.handshaking_servers_count(),
                finished_init = state.has_finished_init(),
                config_count = state.configs.len(),
                "gather_mcp_servers: snapshotting MCP state for user preamble render"
            );
            state
                .all_clients()
                .map(|(n, c)| (n.clone(), std::sync::Arc::clone(c)))
                .collect()
        };
        let mut entries: Vec<McpServerEntry> = Vec::with_capacity(clients.len());
        for (name, client) in &clients {
            let instructions = client.server_instructions().await;
            let server_dir = mcps_root
                .as_deref()
                .map(|root| crate::session::mcp_descriptors::server_descriptor_dir(root, name));
            entries.push(McpServerEntry {
                name: name.clone(),
                server_use_instructions: instructions.filter(|s: &String| !s.trim().is_empty()),
                folder_path: server_dir.map(|d| d.to_string_lossy().to_string()),
            });
        }
        let gateway_entries = self.gather_gateway_mcp_servers(mcps_root.as_deref()).await;
        entries.extend(gateway_entries);
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }
    async fn gather_gateway_mcp_servers(
        &self,
        mcps_root: Option<&std::path::Path>,
    ) -> Vec<echo_agent_core::prompt::user_message::McpServerEntry> {
        use echo_agent_core::prompt::user_message::McpServerEntry;
        let disabled_gateway_tools = crate::util::config::get_all_mcp_disabled_tools(
            std::path::Path::new(&self.session_info.cwd),
        );
        let catalog = {
            let state = self.managed_mcp_handle.lock().await;
            if state.gateway_tools_active {
                match &state.gateway_tool_cache {
                    crate::session::managed_mcp::GatewayToolCatalogCache::Ready(catalog) => {
                        Some(catalog.clone())
                    }
                    _ => None,
                }
            } else {
                None
            }
        };
        let Some(catalog) = catalog else {
            return Vec::new();
        };
        let mut connectors = std::collections::BTreeMap::<String, String>::new();
        let mut gateway_connectors: Vec<String> = catalog
            .tools
            .iter()
            .map(|tool| tool.connector_id.clone())
            .collect();
        gateway_connectors.sort_unstable();
        gateway_connectors.dedup();
        let mut descriptors = Vec::new();
        for tool in &catalog.tools {
            if gateway_tool_is_disabled(tool, &disabled_gateway_tools) {
                continue;
            }
            connectors
                .entry(tool.connector_id.clone())
                .or_insert_with(|| tool.connector_name.clone());
            descriptors.push(crate::session::mcp_descriptors::GatewayToolDescriptor {
                connector_id: tool.connector_id.clone(),
                tool_id: tool.tool_id.clone(),
                description: tool.description.clone(),
                json_schema: tool.json_schema.clone(),
            });
        }
        connectors
            .into_iter()
            .map(|(connector_id, connector_name)| McpServerEntry {
                folder_path: mcps_root.map(|root| {
                    crate::session::mcp_descriptors::server_descriptor_dir(root, &connector_id)
                        .to_string_lossy()
                        .to_string()
                }),
                name: connector_id,
                server_use_instructions: (!connector_name.trim().is_empty())
                    .then_some(connector_name),
            })
            .collect()
    }
    /// Build a `PathRewriter` for sanitizing overlay paths in model-facing text.
    ///
    /// Returns `None` when `display_cwd` is unset (no rewriting needed). Used
    /// by tool-result handlers to rewrite prompt_text, error messages, and any
    /// other model-visible content that may embed the real worktree cwd.
    pub(super) fn path_rewriter(&self) -> Option<crate::session::acp_conversion::PathRewriter> {
        crate::session::acp_conversion::PathRewriter::new(
            &self.session_info.cwd,
            self.display_cwd.get().map(|s| s.as_str()),
        )
    }
    /// If the prompt exceeds LARGE_PROMPT_THRESHOLD, write the full content to a file
    /// and return a truncated version with the local path embedded for the model to read.
    ///
    /// Takes context and query separately to prioritise the query: kept intact
    /// when it fits, else bounded head+tail (trailing question survives).
    ///
    /// Returns `(assembled_message, Some(local_path))` when truncated, or `(assembled, None)`.
    /// Includes skill information in the assembled prompt.
    pub(super) async fn maybe_truncate_large_prompt_with_skills(
        &self,
        context: String,
        query: String,
        skill_information: String,
        is_cursor: bool,
        prompt_index: usize,
    ) -> (String, Option<std::path::PathBuf>) {
        let full_message = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
            &context,
            &query,
            &skill_information,
            is_cursor,
        );
        if full_message.len() <= LARGE_PROMPT_THRESHOLD {
            return (full_message, None);
        }
        let file_path = get_prompt_file_path(&self.session_info, prompt_index);
        let full_len = full_message.len();
        let bounded = build_truncated_prompt_message(
            &context,
            &query,
            &skill_information,
            is_cursor,
            &file_path,
            full_len,
        );
        let join_fallback =
            strip_offload_notice(&bounded, &build_offload_notice(full_len, &file_path));
        let offload = tokio::task::spawn_blocking(move || {
            write_offload_and_build(
                &full_message,
                bounded,
                file_path,
                crate::util::secure_file::write_secure_file,
            )
        })
        .await;
        match offload {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(
                    ?e,
                    full_bytes = full_len,
                    "spawn_blocking join failed for large-prompt offload"
                );
                (join_fallback, None)
            }
        }
    }
    /// Add a followup message from the permission panel as a user turn in the conversation.
    /// This sends the message to the scrollback and adds it to the conversation context.
    pub(super) async fn add_followup_message_as_user_turn(&self, message: &str) {
        self.inject_synthetic_user_message(
            message,
            ConversationItem::user(message.to_string()),
            true,
            &[],
        )
        .await;
    }
    /// Run the image-transcription pipeline for a turn that contains
    /// user-supplied images. Returns the new `user_message` text with the
    /// `<image>` / `<image_files>` envelopes prepended; on any failure
    /// returns an `acp::Error` so the entire turn is aborted (per product
    /// decision -- we never silently drop image context).
    pub(super) async fn transcribe_user_images(
        &self,
        original_user_message: String,
        images: &[agent_client_protocol::ImageContent],
    ) -> Result<String, acp::Error> {
        let prior = self.chat_state_handle.get_conversation().await;
        let outline = crate::session::image_describe::build_conversation_outline(&prior);
        let session_dir = crate::session::persistence::ensure_owner_only_session_dir(
            &crate::session::info::Info {
                id: self.session_info.id.clone(),
                cwd: self.session_info.cwd.clone(),
            },
        )
        .map_err(|e| {
            acp::Error::internal_error().data(format!("failed to create session dir: {e}"))
        })?;
        let persisted = crate::session::image_describe::persist_user_images(&session_dir, images)
            .map_err(|e| {
            acp::Error::internal_error()
                .data(format!("failed to save user images to assets dir: {e}"))
        })?;
        let image_paths: Vec<String> = persisted
            .iter()
            .map(|p| p.path.to_string_lossy().into_owned())
            .collect();
        let current_query = crate::session::image_describe::strip_template_context_tags(
            &echo_agent_chat_state::compaction_utils::extract_user_query(&original_user_message),
        );
        let active_session_config = self.reconstruct_full_config().await;
        let resolved_describe = self
            .resolve_aux_sampler_config(&self.image_description_model)
            .await;
        let (describe_model, sampler_config) =
            crate::agent::config::finalize_image_describe_sampler_config(
                resolved_describe,
                &active_session_config,
                self.client_identifier.clone(),
                Some(self.max_retries),
            );
        let client = echo_agent_sampler::SamplingClient::new(sampler_config).map_err(|e| {
            acp::Error::internal_error().data(format!(
                "failed to build image-describe sampling client: {e}"
            ))
        })?;
        let model = &describe_model;
        let limit = crate::session::image_describe::IMAGE_DESCRIPTION_PROCESSING_LIMIT;
        let skip_count = persisted.len().saturating_sub(limit);
        if skip_count > 0 {
            tracing::info!(
                session_id = %self.session_info.id,
                total = persisted.len(),
                skipped = skip_count,
                limit,
                "image transcription: skipping oldest images due to processing limit",
            );
        }
        let mut description_parts = Vec::with_capacity(persisted.len());
        for (i, p) in persisted.iter().enumerate() {
            let part = if i < skip_count {
                crate::session::image_describe::SKIPPED_IMAGE_MARKER.to_owned()
            } else {
                self.image_describe_cache
                    .get_or_describe(
                        client.clone(),
                        model,
                        &p.raw_bytes,
                        &p.mime_type,
                        outline.as_deref(),
                        &current_query,
                        crate::session::image_describe::ImageDescribeSource::UserAttachment,
                        "",
                    )
                    .await
                    .map_err(|e| {
                        acp::Error::internal_error()
                            .data(format!("image transcription failed: {e}"))
                    })?
            };
            if persisted.len() > 1 {
                description_parts.push(format!("Image {}: {}", i + 1, part));
            } else {
                description_parts.push(part);
            }
        }
        let description = description_parts.join("\n\n");
        Ok(crate::session::image_describe::render_image_user_message(
            &description,
            &image_paths,
            &original_user_message,
        ))
    }
}
