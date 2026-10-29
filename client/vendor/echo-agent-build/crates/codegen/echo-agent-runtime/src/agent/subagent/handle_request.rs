use super::attempt_runner::{
    OneTurnAttemptInput, OneTurnAttemptOutcome, OneTurnTraceCapture, OneTurnUsageInput,
    capture_and_fold_one_turn_usage, run_one_turn_attempt,
};
use super::*;
use crate::upload::trace::PromptMetadataParams;
use echo_agent_sampling_types::ReasoningEffort;
use echo_agent_tools::implementations::{echo_agent_build, opencode};
/// Budget for the pre-completion child transcript flush (replay buffer +
/// persistence to disk). Mirrors the workflow-shutdown persistence bound.
const CHILD_COMPLETION_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
pub(super) fn task_model_override_error(
    requested: Option<&str>,
    provenance: ModelOverrideProvenance,
    is_resume: bool,
    available: &indexmap::IndexMap<String, crate::agent::config::ModelEntry>,
    is_session_auth: bool,
) -> Option<String> {
    if provenance != ModelOverrideProvenance::Tool || is_resume {
        return None;
    }
    let requested = requested?;
    crate::agent::models::task_model_error_for_catalog(requested, available, is_session_auth)
}
/// Runtime adapter for one shell child. Shared lifecycle state is owned by the
/// `echo-agent-tools` coordinator actor and reached only through `reporter`.
#[tracing::instrument(
    name = "subagent.handle_request",
    skip_all,
    fields(
        subagent_id = %run.request.id,
        parent_session_id = %ctx.parent_session_id,
        subagent_type = %run.request.subagent_type,
    )
)]
pub(crate) async fn run_shell_child(
    run: echo_agent_build::task::coordinator::ChildRunRequest<ShellChildRuntime>,
    mut ctx: SubagentSpawnContext,
    gateway: GatewaySender,
) -> ChildRunOutput<ShellCompletionData> {
    let echo_agent_build::task::coordinator::ChildRunRequest {
        mut request,
        cancellation: cancel_token,
        reporter,
        queued_for,
        session_running,
    } = run;
    let start = std::time::Instant::now();
    let spawn_timer = echo_agent_telemetry::subagent_spawn::SubagentSpawnTimer::new_shared();
    use echo_agent_telemetry::subagent_spawn::SubagentSpawnPhase;
    if let Some(queued) = queued_for {
        spawn_timer.record(SubagentSpawnPhase::QueueWait, queued);
    }
    let mut completion_data = ShellCompletionData::from_context(&ctx);
    if request.owner.is_workflow() && cancel_token.is_cancelled() {
        return child_run_output(
            cancelled_result(&request, "Subagent was cancelled"),
            completion_data,
            None,
        );
    }
    let Some(mut definition) = resolve_agent_definition(&request.subagent_type, &ctx) else {
        let msg = format!("Unknown subagent type: {}", request.subagent_type);
        return child_run_output(failure_result(&request, &msg), completion_data, None);
    };
    match gate_subagent_type(&request.subagent_type, &ctx) {
        SubagentValidateTypeOutcome::Disabled => {
            let msg = format!(
                "Subagent '{}' is disabled via [subagents.toggle] in config.toml",
                request.subagent_type
            );
            return child_run_output(failure_result(&request, &msg), completion_data, None);
        }
        SubagentValidateTypeOutcome::NotAllowed { allowed } => {
            let msg = format!(
                "agent can only spawn: {}; '{}' not allowed",
                allowed.join(", "),
                request.subagent_type
            );
            return child_run_output(failure_result(&request, &msg), completion_data, None);
        }
        SubagentValidateTypeOutcome::Unknown { .. }
        | SubagentValidateTypeOutcome::ValidationUnavailable => {
            let msg = format!("Cannot validate subagent '{}'", request.subagent_type);
            return child_run_output(failure_result(&request, &msg), completion_data, None);
        }
        SubagentValidateTypeOutcome::Ok => {}
        _ => {
            let msg = format!("Cannot validate subagent '{}'", request.subagent_type);
            return child_run_output(failure_result(&request, &msg), completion_data, None);
        }
    }
    resolve_subagent_toolset(
        &request.subagent_type,
        request.runtime_overrides.harness_agent_type.as_deref(),
        &ctx,
        &mut definition,
    );
    let cwd = ctx
        .parent_session_info
        .as_ref()
        .map(|i| std::path::Path::new(&i.cwd));
    let mut effective_runtime = echo_agent_subagent_resolution::resolve_runtime_config(
        &request.subagent_type,
        &request.runtime_overrides,
        &ctx.subagent_roles,
        &ctx.subagent_personas,
        cwd,
        &definition,
    );
    if let Some(authority) = ctx
        .parent_turn_authority
        .as_ref()
        .filter(|authority| authority.document_tools || authority.prompt_mode.is_read_only())
    {
        // Resolve policy before any resume/worktree setup can mutate a project.
        // All children operate in the exact parent workspace under its run.
        if effective_runtime.isolation != echo_agent_tool_types::SubagentIsolationMode::None
            || request.resume_from.is_some()
            || request
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd != authority.workspace)
            || ctx.parent_cwd.to_string_lossy() != authority.workspace
        {
            return child_run_output(
                failure_result(
                    &request,
                    "This turn requires a fresh subagent in the same editor-owned workspace; resume, alternate cwd and worktree isolation are unavailable",
                ),
                completion_data,
                None,
            );
        }
        request.run_in_background = false;
        request.await_to_completion = true;
        // Definition-level background defaults must not outlive this grant.
        definition.background = Some(false);
    }
    let prompt = request.prompt.clone();
    if let Some(ref err) = effective_runtime.persona_error {
        tracing::error!(
            subagent_id = %request.id,
            error = err,
            "Persona resolution failed, aborting subagent spawn"
        );
        return child_run_output(failure_result(&request, err), completion_data, None);
    }
    if let Some(ref warn) = effective_runtime.role_prompt_warning {
        tracing::warn!(
            subagent_id = %request.id,
            warning = warn,
            "Role prompt_file degraded, continuing without role prompt"
        );
    }
    let resume_source = if let Some(resume_id) = request
        .resume_from
        .as_deref()
        .filter(|s| is_valid_resume_id(s))
    {
        match reporter
            .resume_source(resume_id, &ctx.parent_session_id)
            .await
        {
            SubagentResumeLookup::Active => {
                let msg = format!(
                    "Cannot resume from subagent '{resume_id}': it is still running. \
                     Wait for it to complete before resuming."
                );
                return child_run_output(failure_result(&request, &msg), completion_data, None);
            }
            SubagentResumeLookup::Completed(info) => Some(ResumeSourceData {
                subagent_id: info.subagent_id,
                child_session_id: info.child_session_id,
                child_cwd: info.child_cwd,
                worktree_path: info.worktree_path.map(PathBuf::from),
                snapshot_ref: info.snapshot_ref,
                subagent_type: info.subagent_type,
                persona: info.persona,
                model_id: info.model_id,
            }),
            SubagentResumeLookup::Missing => {
                match durable_resume_source_for(resume_id, &ctx.parent_session_id, &ctx.parent_cwd)
                {
                    Some(info) => Some(info),
                    None => {
                        let msg = format!(
                            "Cannot resume from subagent '{resume_id}': not found. \
                             The subagent may have been evicted or the ID is invalid."
                        );
                        return child_run_output(
                            failure_result(&request, &msg),
                            completion_data,
                            None,
                        );
                    }
                }
            }
        }
    } else {
        None
    };
    if let Some(ref source) = resume_source {
        if request.runtime_overrides.model.is_some() {
            tracing::debug!(
                subagent_id = %request.id,
                "Ignoring caller model override on resume; source model will be pinned"
            );
        }
        effective_runtime.model = None;
        if let Err(e) = echo_agent_subagent_resolution::validate_resume_identity(
            &request.subagent_type,
            request.runtime_overrides.persona.as_deref(),
            source,
        ) {
            return child_run_output(
                failure_result(&request, &e.to_string()),
                completion_data,
                None,
            );
        }
    }
    if let Some(error) = task_model_override_error(
        request.runtime_overrides.model.as_deref(),
        request.runtime_overrides.model_override_provenance,
        resume_source.is_some(),
        &ctx.available_models,
        ctx.auth_manager
            .current_or_expired()
            .is_some_and(|a| a.is_session_auth()),
    ) {
        return child_run_output(failure_result(&request, &error), completion_data, None);
    }
    let worktree_path = if let Some(ref source) = resume_source {
        if effective_runtime.isolation != echo_agent_tool_types::SubagentIsolationMode::None
            && source.worktree_path.is_none()
        {
            tracing::info!(
                subagent_id = %request.id,
                "Ignoring isolation=worktree override: resumed source had no worktree"
            );
        }
        match source.worktree_path.as_deref() {
            None => None,
            Some(dest) => {
                match resume_worktree_action(dest.is_dir(), source.snapshot_ref.as_deref()) {
                    ResumeWorktreeAction::Reuse => Some(dest.to_path_buf()),
                    ResumeWorktreeAction::Rehydrate => {
                        let snapshot_ref = source.snapshot_ref.clone().unwrap_or_default();
                        let source_repo = resolve_subagent_source_repo(&ctx);
                        match crate::session::worktree::rehydrate_subagent_worktree(
                            dest,
                            &source_repo,
                            &snapshot_ref,
                            Some(source.subagent_id.as_str()),
                        )
                        .await
                        {
                            Ok(path) => {
                                tracing::info!(
                                    subagent_id = %request.id,
                                    worktree_path = %path.display(),
                                    snapshot_ref = %snapshot_ref,
                                    "Rehydrated subagent worktree from snapshot for resume"
                                );
                                Some(path)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    subagent_id = %request.id,
                                    error = %e,
                                    "Failed to rehydrate subagent worktree, falling back to shared workspace"
                                );
                                None
                            }
                        }
                    }
                    ResumeWorktreeAction::Shared => {
                        tracing::warn!(
                            subagent_id = %request.id,
                            worktree = %dest.display(),
                            "Resumed subagent worktree dir missing with no snapshot; using shared workspace"
                        );
                        None
                    }
                }
            }
        }
    } else if effective_runtime.isolation != echo_agent_tool_types::SubagentIsolationMode::None {
        let source_cwd = parent_source_cwd(&ctx);
        let dest = match crate::session::worktree::worktree_base_dir_for_source(&source_cwd) {
            Ok(base) => base.join(format!("subagent-{}", request.id)),
            Err(e) => {
                tracing::warn!(
                    subagent_id = %request.id,
                    error = %e,
                    "Could not resolve worktree base dir, using temp dir for subagent worktree"
                );
                std::env::temp_dir()
                    .join("echo-agent-subagent-worktrees")
                    .join(&request.id)
            }
        };
        let source_clone = source_cwd;
        let subagent_id = request.id.clone();
        let creation_mode: echo_agent_fast_worktree::CreationMode = ctx.worktree_type.into();
        let btrfs_delegate = crate::session::worktree::btrfs_delegate_from_env();
        match tokio::task::spawn_blocking(move || {
            let mut builder = echo_agent_fast_worktree::WorktreeBuilder::new(&source_clone, &dest)
                .working_tree_mode(echo_agent_fast_worktree::WorkingTreeMode::PreserveWorkingTree)
                .creation_mode(creation_mode)
                .worktree_kind(echo_agent_fast_worktree::WorktreeKind::Subagent)
                .session_id(subagent_id);
            if let Some(delegate) = btrfs_delegate {
                builder = builder.btrfs_delegate(delegate);
            }
            builder.create()
        })
        .await
        {
            Ok(Ok(report)) => {
                tracing::info!(
                    subagent_id = %request.id,
                    worktree_path = %report.worktree_path.display(),
                    commit = %report.commit,
                    "Created isolated worktree for subagent"
                );
                Some(report.worktree_path)
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    subagent_id = %request.id,
                    error = %e,
                    "Failed to create worktree, falling back to shared workspace"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    subagent_id = %request.id,
                    error = %e,
                    "Worktree creation task panicked, falling back to shared workspace"
                );
                None
            }
        }
    } else {
        None
    };
    let worktree_freshly_created = resume_source.is_none() && worktree_path.is_some();
    if let Some(raw_cwd) = request.cwd.as_deref() {
        match sanitize_cwd_value(raw_cwd) {
            Some(cwd_path) => {
                if worktree_path.is_none() && resume_source.is_none() {
                    let p = Path::new(&cwd_path);
                    if !p.is_dir() {
                        let msg = if p.exists() {
                            format!("cwd \"{cwd_path}\" exists but is not a directory")
                        } else {
                            format!("cwd \"{cwd_path}\" does not exist")
                        };
                        return child_run_output(
                            failure_result(&request, &msg),
                            completion_data,
                            None,
                        );
                    }
                }
                request.cwd = Some(cwd_path);
            }
            None => request.cwd = None,
        }
    }
    if effective_runtime.reasoning_effort.is_some() || effective_runtime.capability_mode.is_some() {
        tracing::info!(
            subagent_id = %request.id,
            reasoning_effort = ?effective_runtime.reasoning_effort,
            capability_mode = ?effective_runtime.capability_mode,
            "Resolved runtime overrides for subagent"
        );
    }
    effective_runtime.capability_mode = echo_agent_subagent_resolution::intersect_capability_modes(
        effective_runtime.capability_mode,
        definition.capability_mode,
    );
    let child_depth = request
        .runtime_overrides
        .spawn_depth
        .unwrap_or(ctx.parent_depth + 1);
    let tools_before_policy = definition.tool_config.tools.len();
    let allow_nested_subagents = child_depth < ctx.subagents_max_depth;
    echo_agent_subagent_resolution::apply_child_tool_policy(
        &mut definition,
        effective_runtime.capability_mode,
        allow_nested_subagents,
    );
    if let Some(mode) = effective_runtime.capability_mode {
        tracing::info!(
            subagent_id = %request.id,
            capability_mode = ?mode,
            tools_remaining = definition.tool_config.tools.len(),
            "Applied capability mode filter to agent tool config"
        );
    }
    if !allow_nested_subagents && definition.tool_config.tools.len() < tools_before_policy {
        tracing::info!(
            subagent_id = %request.id,
            child_depth,
            "Stripped task tool from child at max depth"
        );
    }
    if request.owner.is_workflow() {
        definition.tool_config.tools.retain(|tool| {
            !matches!(
                tool.id.rsplit(':').next(),
                Some("scheduler_create" | "scheduler_list" | "scheduler_delete")
            )
        });
    }
    if request.fork_context {
        effective_runtime.model = Some(ctx.model_id.0.to_string());
    }
    let (mut effective_sampling_config, mut effective_model_id) = resolve_effective_model_config(
        effective_runtime.model.as_deref(),
        &request.subagent_type,
        &definition.model,
        &ctx,
    )
    .await;
    let subagent_max_turns = resolve_subagent_max_turns(definition.max_turns, ctx.parent_max_turns);
    {
        let model_str = &effective_sampling_config.model;
        let model_unknown = !model_str.is_empty()
            && !ctx.available_models.is_empty()
            && !ctx.available_models.contains_key(model_str)
            && !ctx
                .available_models
                .values()
                .any(|e| e.info().model == *model_str);
        if model_unknown {
            let (parent_config, parent_mid) = read_parent_sampling_config(&ctx).await;
            tracing::warn!(
                subagent_id = %request.id,
                resolved_model = %model_str,
                parent_model = %parent_config.model,
                "Resolved subagent model not found in available models — \
                 falling back to parent model"
            );
            effective_sampling_config = parent_config;
            effective_model_id = parent_mid;
        }
    }
    if let Some(ref source) = resume_source
        && let Some(ref source_model) = source.model_id
        && effective_model_id.0.as_ref() != source_model.as_str()
    {
        if let Some(resolved) = resolve_model_override_to_config(source_model, &ctx) {
            tracing::info!(
                subagent_id = %request.id,
                resolved_model = %effective_model_id.0,
                source_model = source_model,
                "Pinning resumed child to source model"
            );
            effective_sampling_config = resolved.0;
            effective_model_id = resolved.1;
        } else {
            let msg = format!(
                "Cannot resume from subagent '{}': source model '{source_model}' \
                 is no longer available in the model catalogue.",
                source.subagent_id,
            );
            return child_run_output(failure_result(&request, &msg), completion_data, None);
        }
    }
    if let Some(raw) = effective_runtime.reasoning_effort.as_deref()
        && ctx
            .models_manager
            .model_supports_reasoning_effort(effective_model_id.0.as_ref())
    {
        match raw.parse::<ReasoningEffort>() {
            Ok(eff) => effective_sampling_config.reasoning_effort = Some(eff),
            Err(err) => {
                tracing::warn!(
                    value = raw,
                    error = %err,
                    "subagent reasoning_effort: parse failed, ignoring override"
                )
            }
        }
    }
    let subagent_id = request.id.clone();
    let child_session_id = acp::SessionId::new(subagent_id.clone());
    let override_cwd = select_override_cwd(resume_source.as_ref(), request.cwd.as_deref());
    let effective_cwd = resolve_child_cwd(worktree_path.as_deref(), override_cwd, &ctx.parent_cwd)
        .to_string_lossy()
        .into_owned();
    let child_session_info = SessionInfo {
        id: child_session_id.clone(),
        cwd: effective_cwd,
    };
    let child_session_dir = session::persistence::session_dir(&child_session_info);
    if let Err(e) = crate::util::echo_agent_home::ensure_sessions_cwd_dir(&child_session_info.cwd) {
        tracing::warn!(?e, "failed to ensure sessions cwd dir for subagent session");
    }
    let parent_session_dir = session::persistence::session_dir(&SessionInfo {
        id: acp::SessionId::new(ctx.parent_session_id.clone()),
        cwd: ctx.parent_cwd.to_string_lossy().to_string(),
    });
    let subagent_meta_dir = parent_session_dir.join("subagents").join(&subagent_id);
    let InitialContext {
        source: context_source,
        copy_error: fork_copy_error,
        prefix_len: inherited_prefix_len,
        conversation: forked_conversation,
        verbatim_fork: context_verbatim_fork,
    } = match bootstrap_initial_context(
        &request,
        resume_source.as_ref(),
        &ctx,
        &child_session_info,
        &child_session_dir,
        effective_model_id.0.as_ref(),
        effective_sampling_config.context_window,
    )
    .await
    {
        BootstrapInitialContext::Ready(ctx) => ctx,
        BootstrapInitialContext::ResumeAbort(msg) => {
            tracing::error!(
                subagent_id = %request.id,
                error = %msg,
                "Resume-copy failed, aborting subagent spawn"
            );
            return child_run_output(failure_result(&request, &msg), completion_data, None);
        }
    };
    let verbatim_mirror_fork =
        context_source == InitialContextSource::Forked && context_verbatim_fork;
    let task_prompt_text = prompt.clone();
    let (mut forked_conversation, mut inherited_prefix_len) =
        (forked_conversation, inherited_prefix_len.unwrap_or(0));
    if context_source != InitialContextSource::Resumed
        && !verbatim_mirror_fork
        && let Some(ref pi) = effective_runtime.persona_instructions
    {
        let reminder = echo_agent_sampling_types::conversation::ConversationItem::system_reminder(
            format!("<system-reminder>\n{pi}\n</system-reminder>"),
        );
        let insert_at = inherited_prefix_len.min(forked_conversation.len());
        forked_conversation.insert(insert_at, reminder);
        inherited_prefix_len += 1;
    }
    let effective_source_str = match &context_source {
        InitialContextSource::New => "new",
        InitialContextSource::Forked => "forked",
        InitialContextSource::Resumed => "resumed",
    };
    let subagent_meta = SubagentMeta {
        subagent_id: subagent_id.clone(),
        parent_session_id: ctx.parent_session_id.clone(),
        child_session_id: child_session_id.0.to_string(),
        subagent_type: request.subagent_type.clone(),
        description: request.description.clone(),
        prompt: request.prompt.clone(),
        status: "running".to_string(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        error: None,
        effective_context_source: Some(effective_source_str.to_string()),
        context_normalized: fork_context_normalized(&context_source, context_verbatim_fork),
        fork_copy_error: fork_copy_error.clone(),
        persona: effective_runtime.persona.clone(),
        resumed_from: request.resume_from.clone(),
        child_cwd: Some(child_session_info.cwd.clone()),
        worktree_path: worktree_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        snapshot_ref: None,
        effective_model_id: Some(effective_model_id.0.to_string()),
    };
    write_subagent_meta(&subagent_meta_dir, &subagent_meta);
    if let (Some(bucket_url), Some(upload_method)) = (&ctx.gcs_bucket_url, &ctx.gcs_upload_method) {
        let gcs_meta = SubagentSessionMetadata::from_meta(
            &subagent_meta,
            Some(&*effective_model_id.0),
            Some(&child_session_info.cwd),
            None,
            None,
            None,
            effective_runtime.reasoning_effort.as_deref(),
            effective_runtime.role_name.as_deref(),
            request.parent_prompt_id.as_deref(),
            0,
        );
        let bucket = bucket_url.clone();
        let method = upload_method.clone();
        let auth_for_spawn = ctx.auth_manager.clone();
        tokio::spawn(async move {
            upload_subagent_metadata(&gcs_meta, &bucket, method, auth_for_spawn).await;
        });
    }
    let gcs_upload_ctx = GcsUploadContext {
        bucket_url: ctx.gcs_bucket_url.clone(),
        upload_method: ctx.gcs_upload_method.clone(),
        model_id: Some(effective_model_id.0.to_string()),
        cwd: Some(child_session_info.cwd.clone()),
        reasoning_effort: effective_runtime.reasoning_effort.clone(),
        role_name: effective_runtime.role_name.clone(),
        parent_prompt_id: request.parent_prompt_id.clone(),
        auth_manager: ctx.auth_manager.clone(),
        isolation_mode: Some(format!("{:?}", effective_runtime.isolation)),
        capability_mode: effective_runtime
            .capability_mode
            .as_ref()
            .map(|m| format!("{m:?}")),
        depth: child_depth,
    };
    emit_subagent_notification(
        &gateway,
        &ctx.parent_session_id,
        SessionUpdate::SubagentSpawned {
            subagent_id: subagent_id.clone(),
            child_session_id: child_session_id.0.to_string(),
            parent_session_id: ctx.parent_session_id.clone(),
            parent_prompt_id: request.parent_prompt_id.clone(),
            subagent_type: request.subagent_type.clone(),
            description: request.description.clone(),
            effective_context_source: Some(effective_source_str.to_string()),
            context_normalized: fork_context_normalized(&context_source, context_verbatim_fork),
            capability_mode: effective_runtime
                .capability_mode
                .and_then(|m| serde_json::to_value(m).ok())
                .and_then(|v| v.as_str().map(String::from)),
            persona: effective_runtime.persona.clone(),
            role: effective_runtime.role_name.clone(),
            model: Some(effective_model_id.0.to_string()),
            resumed_from: request.resume_from.clone(),
            workflow_run_id: request.owner.workflow_run_id().map(str::to_string),
        },
        ctx.parent_cmd_tx.as_ref(),
    );
    completion_data.spawned_notification_emitted = true;
    let early_gcs_ctx = GcsUploadContext {
        bucket_url: ctx.gcs_bucket_url.clone(),
        upload_method: ctx.gcs_upload_method.clone(),
        model_id: None,
        cwd: None,
        isolation_mode: None,
        capability_mode: None,
        reasoning_effort: effective_runtime.reasoning_effort.clone(),
        role_name: effective_runtime.role_name.clone(),
        parent_prompt_id: request.parent_prompt_id.clone(),
        depth: 0,
        auth_manager: ctx.auth_manager.clone(),
    };
    let sampling_client = match crate::sampling::Client::new(effective_sampling_config.clone()) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Sampling client error: {e}");
            let result = fail_subagent(
                &msg,
                &subagent_id,
                &child_session_id,
                &subagent_meta_dir,
                0,
                &early_gcs_ctx,
            );
            return child_run_output(result, completion_data, None);
        }
    };
    let persistence = match session::persistence::new_with_explicit_dir(
        &child_session_info,
        child_session_dir.clone(),
        effective_model_id.clone(),
        sampling_client,
        effective_sampling_config.model.clone(),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("Persistence error: {e}");
            let result = fail_subagent(
                &msg,
                &subagent_id,
                &child_session_id,
                &subagent_meta_dir,
                0,
                &early_gcs_ctx,
            );
            return child_run_output(result, completion_data, None);
        }
    };
    let child_cwd = resolve_child_cwd(worktree_path.as_deref(), override_cwd, &ctx.parent_cwd);
    let covered_by_parent = echo_agent_fsnotify::watch_root_covers(&ctx.parent_cwd, &child_cwd);
    let subagent_fs_watch = FsWatchCapabilities {
        hunk_tracking: ctx.hunk_tracking_enabled && !covered_by_parent,
        ..FsWatchCapabilities::none()
    };
    let child_cwd_abs = echo_agent_paths::AbsPathBuf::new(child_cwd).unwrap_or_else(|_| {
        echo_agent_paths::AbsPathBuf::new(std::env::current_dir().unwrap_or_default())
            .expect("current_dir should be absolute")
    });
    let mut tool_ctx = ToolContext::with_preloaded_env(
        child_cwd_abs,
        Some(gateway.clone()),
        Some(child_session_id.clone()),
        ctx.fs.clone(),
        ctx.terminal.clone(),
        ctx.hunk_tracker_handle.clone(),
        (*ctx.session_env).clone(),
    )
    .with_hunk_tracking_enabled(ctx.hunk_tracking_enabled);
    tool_ctx.subagent_event_tx = Some(ctx.subagent_event_tx.clone());
    let task_output_budget = request
        .runtime_overrides
        .output_token_budget
        .map(crate::tools::tool_context::TaskOutputTokenBudget::limited);
    tool_ctx.task_output_token_budget = task_output_budget.clone();
    tool_ctx.sampler_retry_only_before_output = task_output_budget.is_some();
    tool_ctx.monitor_event_buffer = Some(MonitorEventBuffer::default());
    tool_ctx.subagent_depth = child_depth;
    tool_ctx.lsp = ctx.lsp.clone();
    tool_ctx.process_scope = ctx.process_scope.clone();
    let parent_traceparent = echo_agent_file_utils::trace_context::current_traceparent();
    let tracker_child_cwd = child_session_info.cwd.clone();
    let tracker_model_id = effective_model_id.0.to_string();
    let initial_child_tokens =
        echo_agent_chat_state::estimate_conversation_tokens(&forked_conversation);
    let model_entry = crate::agent::config::find_model_by_id(
        &ctx.available_models,
        effective_model_id.0.as_ref(),
    );
    let model_has_own_creds = model_entry.is_some_and(|entry| entry.has_own_credentials());
    let inherited_auth_type = subagent_auth_type(model_entry, &ctx.auth_method_id);
    let credentials = echo_agent_chat_state::Credentials {
        api_key: effective_sampling_config.api_key.clone(),
        auth_type: inherited_auth_type,
        alpha_test_key: ctx.alpha_test_key.clone(),
        client_version: effective_sampling_config.client_version.clone(),
    };
    echo_agent_telemetry::unified_log::info(
        "subagent spawn credentials",
        None,
        Some(serde_json::json!({
            "subagent_id": &request.id,
            "subagent_type": &request.subagent_type,
            "effective_model": effective_model_id.0.as_ref(),
            "effective_model_raw": &effective_sampling_config.model,
            "base_url": &effective_sampling_config.base_url,
            "key_prefix": key_prefix(&effective_sampling_config.api_key),
            "auth_type": format!("{:?}", inherited_auth_type),
            "model_has_own_creds": model_has_own_creds,
            "auth_method_id": ctx.auth_method_id.0.as_ref(),
            "parent_model": ctx.model_id.0.as_ref(),
            "parent_key_prefix": key_prefix(&ctx.sampling_config.api_key),
            "context_window": effective_sampling_config.context_window,
        })),
    );
    let attribution_callback: Option<echo_agent_sampler::SharedAttributionCallback> =
        effective_sampling_config.attribution_callback.clone();
    let agent_memory_scope = definition.memory;
    let agent_name_for_memory = definition.name.clone();
    let is_plugin_agent = definition.plugin_name.is_some();
    let yolo_policy_block = echo_agent_workspace::permission::resolution::yolo_disabled_by_policy();
    let agent_permission_mode = resolve_subagent_permission_mode(
        definition.permission_mode.clone(),
        is_plugin_agent,
        yolo_policy_block,
    );
    if agent_permission_mode != definition.permission_mode {
        if is_plugin_agent {
            tracing::warn!(
                agent = %definition.name,
                plugin = ?definition.plugin_name,
                "ignoring permissionMode on plugin agent (not supported for security)"
            );
        } else {
            tracing::warn!(
                agent = %definition.name,
                "ignoring subagent permissionMode=bypassPermissions: always-approve disabled by managed policy"
            );
        }
    }
    if let Some(scope) = agent_memory_scope {
        let memory_tools: Vec<echo_agent_tools::registry::types::ToolConfig> = vec![
            (&echo_agent_build::ReadFileTool).into(),
            (&echo_agent_build::SearchReplaceTool).into(),
            (&opencode::OpenCodeWriteTool).into(),
        ];
        for tc in memory_tools {
            if !definition.tool_config.tools.iter().any(|t| t.id == tc.id) {
                definition.tool_config.tools.push(tc);
            }
        }
        let resolved_mem = scope.resolve_dir(&agent_name_for_memory, &ctx.parent_cwd);
        let memory_dir = &resolved_mem.path;
        let memory_md = memory_dir.join("MEMORY.md");
        if memory_md.is_file()
            && let Ok(content) = std::fs::read_to_string(&memory_md)
        {
            const MAX_LINES: usize = 200;
            const MAX_BYTES: usize = 25 * 1024;
            let truncated: String = content
                .lines()
                .take(MAX_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            let truncated =
                echo_agent_tools::util::truncate::truncate_str(&truncated, MAX_BYTES).to_string();
            if !truncated.is_empty() {
                let injection = format!(
                    "\n\n<agent-memory>\nMemory directory: {}\n\n{truncated}\n</agent-memory>",
                    memory_dir.display()
                );
                definition.prompt_body =
                    Some(definition.prompt_body.unwrap_or_default() + injection.as_str());
            }
        }
    }
    let is_plugin_agent = definition.plugin_name.is_some();
    if let Some(ref hooks_config) = definition.hooks {
        if is_plugin_agent {
            tracing::warn!(
                agent = %definition.name,
                plugin = ?definition.plugin_name,
                "ignoring hooks on plugin agent (not supported for security)"
            );
        } else if !crate::agent::folder_trust::agent_inline_hooks_allowed(definition.scope, || {
            crate::agent::folder_trust::project_scope_allowed(&ctx.parent_cwd)
        }) {
            tracing::warn!(
                agent = %definition.name,
                "ignoring hooks on untrusted project agent (folder not trusted; re-run with --trust)"
            );
        } else {
            let hooks_val = hooks_config.as_value();
            let (specs, errors) = echo_agent_hooks::config::parse_hooks_from_value_with_dir(
                &hooks_val,
                &format!(
                    "{}{}",
                    echo_agent_hooks::config::AGENT_HOOK_PREFIX,
                    definition.name
                ),
                &ctx.parent_cwd,
            );
            for e in &errors {
                tracing::warn!(agent = %definition.name, error = ?e, "agent hook parse error");
            }
            if !specs.is_empty() {
                let specs: Vec<_> = specs
                    .into_iter()
                    .map(|mut s| {
                        if s.event == echo_agent_hooks::event::HookEventName::Stop {
                            s.event = echo_agent_hooks::event::HookEventName::SubagentStop;
                        }
                        s
                    })
                    .collect();
                let mut registry = ctx
                    .hook_registry
                    .as_ref()
                    .map(|r| (**r).clone())
                    .unwrap_or_default();
                registry.append_specs(specs);
                ctx.hook_registry = Some(std::sync::Arc::new(registry));
            }
        }
    }
    let agent_mcp_servers: Vec<_> = if !agent_owned_mcp_servers_allowed(is_plugin_agent) {
        if !definition.mcp_servers.is_empty() {
            tracing::warn!(
                agent = %definition.name,
                plugin = ?definition.plugin_name,
                "ignoring mcpServers on plugin agent (not supported for security)"
            );
        }
        vec![]
    } else {
        definition
                .mcp_servers
                .iter()
                .filter_map(|entry| match entry {
                    echo_agent_core::config::McpServerRef::Named(name) => {
                        ctx.parent_mcp_configs
                            .iter()
                            .find(|s| {
                                crate::session::mcp_servers::mcp_server_name(s) == name
                            })
                            .cloned()
                            .or_else(|| {
                                tracing::warn!(agent = %definition.name, server = name, "mcpServers: named ref not found in parent");
                                None
                            })
                    }
                    echo_agent_core::config::McpServerRef::Inline { name, config } => {
                        if let serde_json::Value::Object(obj) = config
                            && obj.contains_key("type")
                        {
                            let mut flat = obj.clone();
                            flat.insert(
                                "name".to_string(),
                                serde_json::Value::String(name.clone()),
                            );
                            if let Ok(server) = serde_json::from_value::<
                                agent_client_protocol::McpServer,
                            >(serde_json::Value::Object(flat)) {
                                return Some(server);
                            }
                            tracing::debug!(agent = %definition.name, server = name, "ACP wire format parse failed, trying map-keyed");
                        }
                        if let Some(inner_obj) = config.as_object() {
                            let mut flat = inner_obj.clone();
                            flat.insert(
                                "name".to_string(),
                                serde_json::Value::String(name.clone()),
                            );
                            if let Ok(server) = serde_json::from_value::<
                                agent_client_protocol::McpServer,
                            >(serde_json::Value::Object(flat)) {
                                return Some(server);
                            }
                        }
                        tracing::warn!(agent = %definition.name, server = name, "mcpServers: inline config could not be parsed");
                        None
                    }
                })
                .collect()
    };
    let parent_mcp_pool =
        resolve_inherited_mcp_pool(ctx.parent_mcp_pool.take(), &definition.mcp_inheritance);
    let mcp_inherited_count = parent_mcp_pool
        .as_ref()
        .map(|p| p.len() as u32)
        .unwrap_or(0);
    if mcp_inherited_count > 0 {
        tracing::info!(
            subagent_id = %request.id,
            mcp_count = mcp_inherited_count,
            "Subagent inherited MCP servers from parent pool"
        );
    }
    let inherit_skills = definition.inherit_skills;
    let definition_background = definition.background.unwrap_or(false);
    if inherit_skills && ctx.parent_skills.is_none() {
        let parent_cwd_str = ctx.parent_cwd.to_string_lossy().to_string();
        ctx.parent_skills = Some(
            echo_agent_core::prompt::skills::list_skills_with_plugins(
                Some(&parent_cwd_str),
                &ctx.parent_skills_config,
                ctx.plugin_registry.as_deref(),
                ctx.parent_compat,
            )
            .await,
        );
    }
    let skills_inherited_count = if inherit_skills {
        ctx.parent_skills
            .as_ref()
            .map(|s| s.len() as u32)
            .unwrap_or(0)
    } else {
        0
    };
    if skills_inherited_count > 0 {
        tracing::info!(
            subagent_id = %request.id,
            skills_count = skills_inherited_count,
            "Subagent inherited skills from parent"
        );
    }
    let mcp_owned_count = agent_mcp_servers.len() as u32;
    let _active = echo_agent_telemetry::activity::SUBAGENTS_ACTIVE.enter();
    debug_assert!(
        echo_agent_telemetry::activity::SUBAGENTS_ACTIVE.get() >= 1,
        "SubagentLaunched must stamp a self-inclusive count"
    );
    echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::SubagentLaunched {
        subagent_id: request.id.clone(),
        parent_session_id: request.parent_session_id.clone(),
        subagent_type: request.subagent_type.clone(),
        owner: telemetry_owner_kind(&request),
        workflow_run_id: request.owner.workflow_run_id().map(str::to_string),
        queued_ms: queued_for.map(|queued| u64::try_from(queued.as_millis()).unwrap_or(u64::MAX)),
        session_running: u32::try_from(session_running).unwrap_or(u32::MAX),
        persona: request.runtime_overrides.persona.clone(),
        fork_context: matches!(context_source, InitialContextSource::Forked),
        resume_from: request.resume_from.clone(),
        isolated_worktree: worktree_path.is_some(),
        mcp_inherited_count,
        mcp_owned_count,
        skills_inherited_count,
    });
    let subagent_session_default_agent_profile = Some(definition.name.clone());
    let subagent_model_id = effective_sampling_config.model.clone();
    let _ = persistence
        .tx
        .send(crate::session::persistence::PersistenceMsg::CurrentModel {
            model_id: effective_model_id.clone(),
            agent_name: Some(definition.name.clone()),
            reasoning_effort: Some(effective_sampling_config.reasoning_effort),
        });
    crate::waterfall::mark(&request.id, crate::waterfall::stage::SESSION_SPAWN);
    spawn_timer.record(SubagentSpawnPhase::SpawnPrepare, start.elapsed());
    let bootstrap_started_at = std::time::Instant::now();
    if ctx
        .parent_turn_authority
        .as_ref()
        .is_some_and(|authority| authority.prompt_mode.is_read_only())
    {
        ctx.hook_registry = None;
        ctx.client_hooks = Default::default();
    }
    let spawn_result = session::spawn_session_on_thread(
        child_session_info,
        gateway.clone(),
        effective_sampling_config,
        credentials,
        crate::agent::auth_method::new_shared_auth_method_id(Some(ctx.auth_method_id.clone())),
        Some(ctx.auth_manager.clone()),
        attribution_callback,
        tool_ctx,
        agent_mcp_servers,
        vec![],
        Default::default(),
        parent_mcp_pool,
        Vec::new(),
        true,
        false,
        None,
        persistence,
        forked_conversation,
        None,
        None,
        initial_child_tokens,
        crate::session::StartupHints {
            inherited_turn_authority: ctx.parent_turn_authority.clone(),
            inherited_prefix_len: Some(inherited_prefix_len),
            is_subagent: true,
            non_interactive: ctx.parent_non_interactive,
            parent_session_id: Some(ctx.parent_session_id.clone()),
            subagent_type: Some(request.subagent_type.clone()),
            preserve_inherited_system: verbatim_mirror_fork,
            ..Default::default()
        },
        echo_agent_workspace::permission::ClientType::Generic,
        ctx.resolve_auto_compact_threshold_percent(&subagent_model_id),
        echo_agent_core::DEFAULT_SYSTEM_PROMPT_LABEL.to_string(),
        echo_agent_chat_state::CompactionMode::Summary,
        ctx.resolve_compaction_verbatim_input(),
        ctx.resolve_compaction_tool_choice(),
        false,
        None,
        None,
        std::sync::Arc::new(parking_lot::Mutex::new(
            echo_agent_workspace::file_system::CodebaseIndexManager::new(),
        )),
        false,
        subagent_fs_watch,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
        None,
        None,
        None,
        false,
        false,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        definition,
        subagent_session_default_agent_profile,
        if inherit_skills {
            ctx.parent_skills_config.clone()
        } else {
            echo_agent_core::prompt::skills::SkillsConfig::default()
        },
        if inherit_skills {
            ctx.parent_skills.take()
        } else {
            None
        },
        ctx.parent_compat,
        false,
        None,
        None,
        None,
        Vec::new(),
        None,
        if verbatim_mirror_fork {
            None
        } else if let Some(scope) = agent_memory_scope {
            Some({
                let mut c = ctx.memory_config.clone().unwrap_or_default();
                let resolved = scope.resolve_dir(&agent_name_for_memory, &ctx.parent_cwd);
                c.enabled = true;
                c.root_dir_override = Some(resolved.path);
                c.flat_memory_root = resolved.is_project_scoped;
                c
            })
        } else {
            ctx.memory_config.clone()
        },
        false,
        Default::default(),
        ctx.managed_mcp_state.clone(),
        ctx.managed_mcp_proxy_base_url.clone(),
        effective_model_id,
        ctx.yolo_mode
            || matches!(
                agent_permission_mode,
                echo_agent_core::config::PermissionMode::BypassPermissions
            ),
        false,
        None,
        ctx.inference_idle_timeout_secs,
        None,
        ctx.resolve_subagent_rate_limit_max_attempts(&subagent_model_id),
        ctx.web_search_sampling_config.clone(),
        ctx.web_fetch_config.clone(),
        ctx.image_gen_config.clone(),
        ctx.video_gen_config.clone(),
        ctx.app_builder_deployer_config.clone(),
        ctx.write_file_enabled,
        ctx.goal_enabled,
        ctx.background_workflows_enabled,
        true,
        ctx.subagents_max_depth,
        ctx.workflow_max_concurrent_agents,
        ctx.media_gen_batch_limits,
        ctx.ask_user_question_enabled,
        ctx.client_hooks.clone(),
        None,
        std::collections::HashMap::new(),
        Vec::new(),
        echo_agent_core::prompt::context::PromptAudience::Subagent,
        effective_runtime.role_prompt.clone(),
        None,
        ctx.disable_web_search,
        ctx.backend_tools_enabled,
        ctx.respect_gitignore,
        ctx.path_not_found_hints,
        Default::default(),
        ctx.plugin_registry.clone(),
        None,
        ctx.models_manager.clone(),
        parent_traceparent,
        ctx.permission_handle.clone(),
        ctx.api_key_provider.clone(),
        ctx.image_description_model.clone(),
        ctx.hook_registry.clone(),
        ctx.workspace_ops.clone(),
        vec![],
        ctx.todo_gate,
        std::mem::take(&mut ctx.remote_settings),
        std::mem::take(&mut ctx.laziness_debug_log),
        ctx.parent_terminal_backend.clone(),
        if request.owner.is_workflow() {
            None
        } else {
            ctx.parent_scheduler_handle.clone()
        },
        subagent_max_turns,
        if verbatim_mirror_fork && !request.owner.is_workflow() {
            std::mem::take(&mut ctx.parent_tool_definitions)
        } else {
            None
        },
        false,
        Some(spawn_timer.clone()),
        Some(ctx.subagent_sampling_semaphore.clone()),
    )
    .await;
    crate::waterfall::mark(&request.id, crate::waterfall::stage::SESSION_UP);
    spawn_timer.record(
        SubagentSpawnPhase::SessionBootstrap,
        bootstrap_started_at.elapsed(),
    );
    let session_ready_at = std::time::Instant::now();
    let (child_handle, mut permission_rx, _system_prompt, child_thread) = match spawn_result {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to spawn child session: {e}");
            let result = fail_subagent(
                &msg,
                &subagent_id,
                &child_session_id,
                &subagent_meta_dir,
                start.elapsed().as_millis() as u64,
                &gcs_upload_ctx,
            );
            return child_run_output(result, completion_data, None);
        }
    };
    let promoted = reporter
        .started(StartedChild {
            child_session_id: child_session_id.0.to_string(),
            persona: effective_runtime.persona.clone(),
            resumed_from: request.resume_from.clone(),
            child_cwd: tracker_child_cwd,
            worktree_path: worktree_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            effective_model_id: tracker_model_id.clone(),
            definition_background,
            control: ShellChildRuntime {
                child_handle: child_handle.clone(),
                _child_thread: child_thread,
            },
        })
        .await;
    if !promoted {
        ctx.workspace_ops
            .end_local_session(child_session_id.0.as_ref());
        let result = cancel_pending_shell_child(
            &child_handle.cmd_tx,
            &subagent_id,
            &child_session_id,
            &subagent_meta_dir,
            worktree_path.as_deref(),
            worktree_freshly_created,
            start.elapsed().as_millis() as u64,
            &gcs_upload_ctx,
        )
        .await;
        return child_run_output(result, completion_data, None);
    }
    let _progress_publisher = spawn_progress_publisher(
        child_handle.signals_handle.clone(),
        gateway.clone(),
        ctx.parent_session_id.clone(),
        request.id.clone(),
        child_session_id.0.to_string(),
        start,
        cancel_token.clone(),
        goal_tick_cmd_tx(ctx.goal_enabled, ctx.parent_cmd_tx.as_ref()),
    );
    spawn_timer.record(
        SubagentSpawnPhase::ReadyToFirstTurn,
        session_ready_at.elapsed(),
    );
    let attempt = run_one_turn_attempt(OneTurnAttemptInput {
        child_handle: &child_handle,
        request: &request,
        worktree_path: worktree_path.as_deref(),
        task_prompt_text: &task_prompt_text,
        inherited_tool_overrides: ctx.inherited_tool_overrides.clone(),
        gcs_bucket_url: ctx.gcs_bucket_url.as_deref(),
        gcs_upload_method: ctx.gcs_upload_method.as_ref(),
        cancel_token: cancel_token.clone(),
        child_run_started_at: start,
    })
    .await;
    let OneTurnAttemptOutcome {
        mut result,
        trace,
        cancellation_may_hide_usage,
    } = attempt;
    let OneTurnTraceCapture {
        before_copy_rx,
        child_prompt_id,
        turn_started_at,
        turn_token_totals,
    } = trace;
    if let Some(trace_gcs_config) = gcs_upload_ctx.upload_method.as_ref().map(|method| {
        crate::session::repo_changes::TraceExportConfig {
            bucket_url: gcs_upload_ctx.bucket_url.clone(),
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: Some(format!("{}/turn_0", child_session_id.0)),
            absolute_paths: false,
            archive_name_override: None,
            upload_method: method.clone(),
        }
    }) {
        let (copy_tx, session_copy_rx) = tokio::sync::oneshot::channel();
        let _ = child_handle.cmd_tx.send(SessionCommand::CopyFile {
            respond_to: copy_tx,
        });
        let turn_messages: Option<echo_agent_chat_state::TurnCapture> = {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if child_handle
                .cmd_tx
                .send(SessionCommand::TakeTurnMessages { respond_to: tx })
                .is_ok()
            {
                rx.await.ok().flatten()
            } else {
                None
            }
        };
        let streaming_partial = crate::upload::turn::take_streaming_partial(
            &child_handle.cmd_tx,
            child_prompt_id.clone(),
            result.success,
            gcs_upload_ctx.model_id.clone(),
        )
        .await
        .map(|mut cap| {
            cap.reason = Some(if result.cancelled {
                "subagent_cancel".to_string()
            } else {
                "subagent_non_completed".to_string()
            });
            cap
        });
        let mut permission_events = Vec::new();
        while let Ok(event) = permission_rx.try_recv() {
            permission_events.push(event);
        }
        let trace_ctx = PromptTraceContext {
            gcs_config: trace_gcs_config,
            session_info: child_handle.info.clone(),
            turn_number: 0,
            session_handle: child_handle.clone(),
            session_registry_enabled: false,
            upload_queue: None,
            artifact_tracker: crate::upload::manifest::new_artifact_tracker(),
            auth_manager: ctx.auth_manager.clone(),
        };
        let session_dir = crate::session::persistence::session_dir(&child_handle.info);
        if let Ok(prompt_bytes) = std::fs::read(session_dir.join("system_prompt.txt")) {
            let gcs_path = format!("{}/system_prompt.txt", child_session_id.0);
            crate::upload::trace::upload_trace_artifact(
                &trace_ctx,
                &prompt_bytes,
                &gcs_path,
                "text/plain",
                "system_prompt",
            )
            .await;
        }
        if let Ok(ctx_bytes) = std::fs::read(session_dir.join("prompt_context.json")) {
            let gcs_path = format!("{}/prompt_context.json", child_session_id.0);
            crate::upload::trace::upload_trace_artifact(
                &trace_ctx,
                &ctx_bytes,
                &gcs_path,
                "application/json",
                "prompt_context",
            )
            .await;
        }
        upload_session_state(
            &trace_ctx,
            "before",
            before_copy_rx,
            crate::upload::turn::UploadWait::Confirm,
        )
        .await;
        let subagent_auth = ctx.auth_manager.current();
        let metadata = PromptMetadata::new(PromptMetadataParams {
            schema_version: GCS_SCHEMA_VERSION.to_string(),
            session_id: child_session_id.0.to_string(),
            turn_number: 0,
            request_id: child_prompt_id.clone(),
            turn_started_at: turn_started_at.clone(),
            user_id: subagent_auth.as_ref().map(|a| a.user_id.clone()),
            user_email: subagent_auth.as_ref().and_then(|a| a.email.clone()),
            team_id: subagent_auth.as_ref().and_then(|a| a.team_id.clone()),
            client_source: Some("subagent".to_string()),
            client_version: ctx.sampling_config.client_version.clone(),
            model: gcs_upload_ctx.model_id.clone().unwrap_or_default(),
            reasoning_effort: child_handle
                .reasoning_effort
                .map(|e| e.as_str().to_string()),
            host_os: std::env::consts::OS.to_string(),
            host_arch: std::env::consts::ARCH.to_string(),
            prompt_has_image: Some(false),
            prompt_was_truncated: Some(false),
            prompt_verbatim: Some(true),
            cwd: Some(child_handle.info.cwd.clone()),
            agent_type: Some(request.subagent_type.clone()),
            shell_version: Some(echo_agent_version::VERSION.to_string()),
            sandbox: local_sandbox_telemetry(),
            ..Default::default()
        });
        upload_metadata(&trace_ctx, metadata).await;
        let resolved_model = child_handle
            .get_model_metadata()
            .await
            .resolved_model_id
            .or_else(|| gcs_upload_ctx.model_id.clone());
        let turn_result_meta = TurnResultMetadata {
            schema_version: "1",
            request_id: child_prompt_id,
            completed: result.success,
            stop_reason: None,
            total_tokens: None,
            input_tokens: turn_token_totals.map(|t| t.0),
            cached_input_tokens: turn_token_totals.map(|t| t.1),
            output_tokens: turn_token_totals.map(|t| t.2),
            error: result.error.clone(),
            finished_at: chrono::Utc::now().to_rfc3339(),
            signals: None,
            turn_delta: None,
            start_prompt_mode: Some(crate::session::plan_mode::PromptMode::Agent.to_string()),
            end_prompt_mode: Some(crate::session::plan_mode::PromptMode::Agent.to_string()),
            resolved_model,
            subagents_spawned: vec![],
        };
        upload_turn_result(
            &trace_ctx,
            &turn_result_meta,
            crate::upload::turn::UploadWait::Confirm,
        )
        .await;
        match complete_prompt_trace(
            trace_ctx,
            permission_events,
            session_copy_rx,
            turn_messages,
            streaming_partial,
            crate::upload::turn::UploadWait::Confirm,
        )
        .await
        {
            Ok(_) => {
                tracing::debug!(
                    subagent_id = %request.id,
                    child_session_id = %child_session_id.0,
                    "Subagent trace artifacts uploaded"
                );
            }
            Err(e) => {
                tracing::warn!(
                    subagent_id = %request.id,
                    error = %e,
                    "Subagent trace upload failed (non-fatal)"
                );
            }
        }
    }
    completion_data.set_persisted_output_dir(persist_subagent_output(&subagent_meta_dir, &result));
    persist_subagent_completion(&subagent_meta_dir, &result, &gcs_upload_ctx);
    let final_status = result.status().to_string();
    let snapshot_dispose_enabled = ctx.resolve_subagent_worktree_snapshot_enabled();
    let telemetry_tokens = if result.tool_calls > 0 || result.success {
        child_handle.chat_state_handle.get_total_tokens().await
    } else {
        0
    };
    completion_data.telemetry_tokens = telemetry_tokens;
    let fold_acked = capture_and_fold_one_turn_usage(
        &mut result,
        OneTurnUsageInput {
            child_handle: &child_handle,
            task_budget_usage: task_output_budget.as_ref().map(|budget| budget.usage()),
            cancellation_may_hide_usage,
            parent_cmd_tx: ctx.parent_cmd_tx.as_ref(),
            parent_prompt_id: request.parent_prompt_id.as_deref(),
        },
    )
    .await;
    if !fold_acked {
        tracing::warn!(
            subagent_id = %request.id,
            parent_prompt_id = ?request.parent_prompt_id,
            "subagent usage not applied; parent bill marked incomplete"
        );
        let sticky_prompt = request.parent_prompt_id.clone();
        let marked_by_parent = if let Some(cmd_tx) = ctx.parent_cmd_tx.as_ref() {
            let (respond_to, ack) = tokio::sync::oneshot::channel();
            if cmd_tx
                .send(
                    crate::session::commands::SessionCommand::MarkSubagentUsageNotApplied {
                        parent_prompt_id: sticky_prompt.clone(),
                        respond_to,
                    },
                )
                .is_ok()
            {
                ack.await.is_ok()
            } else {
                false
            }
        } else {
            false
        };
        if !marked_by_parent && let Some(pid) = sticky_prompt {
            let (respond_to, ack) = tokio::sync::oneshot::channel();
            if ctx
                .subagent_event_tx
                .send(SubagentEvent::MarkUsageNotApplied(
                    SubagentMarkUsageNotAppliedRequest {
                        parent_session_id: ctx.parent_session_id.clone(),
                        prompt_id: pid,
                        respond_to,
                    },
                ))
                .is_ok()
            {
                let _ = ack.await;
            }
        }
    }
    let outcome = if result.success {
        echo_agent_telemetry::events::Outcome::Completed
    } else if result.cancelled {
        echo_agent_telemetry::events::Outcome::Cancelled
    } else {
        echo_agent_telemetry::events::Outcome::Error
    };
    let mut completed = echo_agent_telemetry::events::SubagentCompleted {
        subagent_id: request.id.clone(),
        parent_session_id: request.parent_session_id.clone(),
        owner: telemetry_owner_kind(&request),
        workflow_run_id: request.owner.workflow_run_id().map(str::to_string),
        outcome,
        duration_ms: result.duration_ms,
        tool_calls: result.tool_calls,
        tokens_used: if telemetry_tokens > 0 {
            Some(telemetry_tokens)
        } else {
            None
        },
        queue_wait_ms: None,
        spawn_prepare_ms: None,
        session_bootstrap_ms: None,
        agent_build_ms: None,
        tool_setup_ms: None,
        ready_to_first_turn_ms: None,
    };
    spawn_timer.write_event_phases(&mut completed);
    echo_agent_telemetry::session_ctx::log_event(completed);
    match (
        &ctx.parent_terminal_backend,
        &ctx.parent_notification_handle,
    ) {
        (Some(parent_tb), Some(parent_notif_handle)) => {
            if !request.surface_completion {
                let reparented_task_ids: Vec<String> = parent_tb
                    .list_tasks()
                    .await
                    .into_iter()
                    .filter(|t| {
                        !t.completed && t.owner_session_id.as_deref() == Some(&*child_session_id.0)
                    })
                    .map(|t| t.task_id)
                    .collect();
                if !reparented_task_ids.is_empty()
                    && let Some(cmd_tx) = ctx.parent_cmd_tx.as_ref()
                {
                    let _ = cmd_tx.send(SessionCommand::RecordGoalTurnTaskIds {
                        task_ids: reparented_task_ids,
                    });
                }
            }
            let parent_backend_weak = std::sync::Arc::downgrade(parent_tb);
            parent_tb
                .reparent_notifications(
                    &child_session_id.0,
                    &ctx.parent_session_id,
                    parent_notif_handle.clone(),
                    parent_backend_weak,
                )
                .await;
        }
        (Some(_), None) | (None, Some(_)) => {
            tracing::warn!(
                child_session_id = %child_session_id.0,
                parent_session_id = %ctx.parent_session_id,
                has_terminal_backend = ctx.parent_terminal_backend.is_some(),
                has_notification_handle = ctx.parent_notification_handle.is_some(),
                "skipping reparent_notifications: parent_terminal_backend and \
                 parent_notification_handle must both be Some"
            );
        }
        (None, None) => {}
    }
    {
        let (respond_to, ack) = oneshot::channel();
        if child_handle
            .cmd_tx
            .send(SessionCommand::FlushComplete { respond_to })
            .is_ok()
        {
            match tokio::time::timeout(CHILD_COMPLETION_FLUSH_TIMEOUT, ack).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    tracing::warn!(
                        subagent_id = %request.id,
                        %error,
                        "child transcript flush failed before completion"
                    )
                }
                Ok(Err(_)) => {
                    tracing::warn!(
                        subagent_id = %request.id,
                        "child session dropped the transcript flush ack before completion"
                    )
                }
                Err(_) => {
                    tracing::warn!(
                        subagent_id = %request.id,
                        "child transcript flush timed out before completion"
                    )
                }
            }
        }
    }
    let _ = child_handle.cmd_tx.send(SessionCommand::Shutdown(
        crate::session::ShutdownKind::Graceful,
    ));
    ctx.workspace_ops
        .end_local_session(child_session_id.0.as_ref());
    let mut disposed_snapshot_ref: Option<String> = None;
    let mut worktree_removed = false;
    if let Some(ref wt_path) = worktree_path {
        if snapshot_dispose_enabled {
            let disposal = dispose_worktree_after_completion(
                wt_path,
                &resolve_subagent_source_repo(&ctx),
                &subagent_meta_dir,
                &final_status,
                &request.id,
            )
            .await;
            worktree_removed = disposal.worktree_removed();
            disposed_snapshot_ref = disposal.snapshot_ref().map(str::to_owned);
        } else {
            tracing::info!(
                subagent_id = %request.id,
                worktree_path = %wt_path.display(),
                "Worktree preserved for review"
            );
        }
    }
    if worktree_removed {
        result.worktree_path = None;
    }
    let success = result.success && !result.cancelled;
    let preview = crate::util::truncate(&result.output, 200);
    let level_fn = if success {
        echo_agent_telemetry::unified_log::info
    } else {
        echo_agent_telemetry::unified_log::error
    };
    level_fn(
        if success {
            "subagent completed"
        } else {
            "subagent failed"
        },
        None,
        Some(serde_json::json!({
            "subagent_id": &request.id,
            "subagent_type": &request.subagent_type,
            "effective_model": tracker_model_id,
            "success": success,
            "cancelled": result.cancelled,
            "duration_ms": result.duration_ms,
            "turns": result.turns,
            "tool_calls": result.tool_calls,
            "output_preview": preview,
            "error": &result.error,
        })),
    );
    child_run_output(result, completion_data, disposed_snapshot_ref)
}
/// What the completion path did with a subagent worktree.
pub(crate) enum Disposal {
    /// The gate kept it, or something before the pointer failed. No resume
    /// pointer on purpose: a pointer sends resume down the rehydrate path,
    /// which deletes the directory to rebuild it from a snapshot that lacks
    /// whatever kept it. The snapshot ref stays durable either way.
    Kept,
    /// The pointer reached disk. Whether the removal then succeeded is a
    /// separate question, and resume works in both cases.
    Snapshotted {
        snapshot_ref: String,
        worktree_removed: bool,
    },
}
impl Disposal {
    pub(crate) fn worktree_removed(&self) -> bool {
        matches!(
            self,
            Disposal::Snapshotted {
                worktree_removed: true,
                ..
            }
        )
    }
    pub(crate) fn snapshot_ref(&self) -> Option<&str> {
        match self {
            Disposal::Kept => None,
            Disposal::Snapshotted { snapshot_ref, .. } => Some(snapshot_ref),
        }
    }
}
/// Capture the worktree into a durable ref, ask whether deleting it would lose
/// anything, and only then persist the resume pointer and remove the directory.
/// Capture first and persist before removing, so a crash mid-disposal never
/// strands a snapshot the resume path cannot find.
///
/// The other removal path, `cancel_pending_shell_child`, skips all of this:
/// there the child never ran, so the directory is the one the checkout made.
pub(crate) async fn dispose_worktree_after_completion(
    worktree: &std::path::Path,
    source_repo: &std::path::Path,
    meta_dir: &std::path::Path,
    final_status: &str,
    subagent_id: &str,
) -> Disposal {
    let ref_name = format!("refs/echoagent/subagents/{subagent_id}");
    let snapshot_ref = match crate::session::worktree::snapshot_subagent_worktree(
        worktree,
        source_repo,
        &ref_name,
    )
    .await
    {
        Ok(snapshot_ref) => snapshot_ref,
        Err(e) => {
            tracing::warn!(
                subagent_id = %subagent_id,
                worktree_path = %worktree.display(),
                error = %e,
                "Failed to snapshot subagent worktree; preserving for review"
            );
            return Disposal::Kept;
        }
    };
    let checked_path = worktree.to_path_buf();
    let checked_source_repo = source_repo.to_path_buf();
    let checked_snapshot = snapshot_ref.clone();
    let reclaim = tokio::task::spawn_blocking(move || {
        echo_agent_fast_worktree::reclaimable_after_snapshot(
            &checked_path,
            Some(&checked_source_repo),
            &checked_snapshot,
        )
    })
    .await;
    match reclaim {
        Ok(echo_agent_fast_worktree::Reclaim::Now { .. }) => {}
        Ok(echo_agent_fast_worktree::Reclaim::Keep(reason)) => {
            tracing::info!(
                subagent_id = %subagent_id,
                worktree_path = %worktree.display(),
                %reason,
                "subagent worktree kept: removal would not preserve every byte"
            );
            return Disposal::Kept;
        }
        Ok(echo_agent_fast_worktree::Reclaim::Unnamed(error)) => {
            tracing::warn!(
                subagent_id = %subagent_id,
                worktree_path = %worktree.display(),
                %error,
                "the discarded commits were not named; preserving the worktree"
            );
            return Disposal::Kept;
        }
        Err(error) => {
            tracing::warn!(
                subagent_id = %subagent_id,
                worktree_path = %worktree.display(),
                %error,
                "the safety check did not finish; preserving the worktree"
            );
            return Disposal::Kept;
        }
    }
    if !update_subagent_meta_snapshot_ref(meta_dir, &snapshot_ref, final_status) {
        tracing::warn!(
            subagent_id = %subagent_id,
            worktree_path = %worktree.display(),
            "snapshot_ref not persisted; preserving worktree for resume"
        );
        return Disposal::Kept;
    }
    let worktree_removed = match crate::session::worktree::remove_subagent_worktree(worktree).await
    {
        Ok(()) => {
            tracing::info!(
                subagent_id = %subagent_id,
                worktree_path = %worktree.display(),
                "snapshotted and removed subagent worktree"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                subagent_id = %subagent_id,
                worktree_path = %worktree.display(),
                error = %e,
                "snapshotted subagent worktree but removal failed; ref persisted for resume"
            );
            false
        }
    };
    Disposal::Snapshotted {
        snapshot_ref,
        worktree_removed,
    }
}
