//! Session/plan-mode concern for `SessionActor` (`handle_session_mode`,
//! plan-mode reminders and persistence, active-template detection).
use super::*;
pub(super) fn prompt_mode_from_session_mode_id(session_mode_id: &acp::SessionModeId) -> PromptMode {
    use echo_agent_tools::types::SessionMode;
    match SessionMode::from_id(session_mode_id.0.as_ref()) {
        SessionMode::Plan => PromptMode::Plan,
        SessionMode::Ask => PromptMode::Ask,
        SessionMode::Default | SessionMode::BrowserUse | SessionMode::ComputerUse => {
            PromptMode::Agent
        }
    }
}
/// Inverse of [`prompt_mode_from_session_mode_id`]: the mode id a client
/// displays for a prompt mode. Needed wherever a transition the client did not
/// drive has to be reported back to it.
pub(super) fn session_mode_id_from_prompt_mode(prompt_mode: PromptMode) -> acp::SessionModeId {
    use echo_agent_tools::types::SessionMode;
    let mode = match prompt_mode {
        PromptMode::Plan => SessionMode::Plan,
        PromptMode::Ask => SessionMode::Ask,
        PromptMode::Agent => SessionMode::Default,
    };
    acp::SessionModeId::new(mode.as_id())
}
/// Pass-through twin: no toolset in this build carries a plan-gated tool.
pub(super) fn filter_cursor_tools_by_plan_mode(
    defs: Vec<ToolDefinition>,
    _plan_active: bool,
) -> Vec<ToolDefinition> {
    defs
}
/// Read-only turns use a closed capability set. New/unknown tools fail closed;
/// shell, MCP, dynamic dispatch and mode-changing tools are excluded. Task
/// children inherit the immutable authority and are bound to the parent turn.
/// Tool names are resolved from registry metadata, so aliases cannot widen it.
pub(super) fn readonly_tool_kind_allowed(
    kind: Option<echo_agent_tools::types::tool::ToolKind>,
) -> bool {
    use echo_agent_tools::types::tool::ToolKind;
    matches!(
        kind,
        Some(
            ToolKind::Read
                | ToolKind::ListDir
                | ToolKind::Search
                | ToolKind::MemorySearch
                | ToolKind::MemoryGet
                | ToolKind::AskUser
                | ToolKind::Task
        )
    )
}

pub(super) fn readonly_tool_input_allowed(input: &ToolInput) -> bool {
    if let ToolInput::Task(task) = input {
        return task.cwd.is_none()
            && task.resume_from.is_none()
            && matches!(
                task.isolation,
                None | Some(echo_agent_tool_types::SubagentIsolationMode::None)
            );
    }
    matches!(
        input,
        ToolInput::ReadFile(_)
            | ToolInput::ListDir(_)
            | ToolInput::Grep(_)
            | ToolInput::CodexReadFile(_)
            | ToolInput::CodexListDir(_)
            | ToolInput::CodexGrepFiles(_)
            | ToolInput::MemorySearch(_)
            | ToolInput::MemoryGet(_)
            | ToolInput::AskUserQuestion(_)
    )
}

impl SessionActor {
    /// Immutable turn-start authority is the ceiling even if a model tool,
    /// custom rule or a next-turn mode selector changes the displayed mode.
    pub(super) fn is_readonly_turn(&self) -> bool {
        self.turn_start_prompt_mode.lock().is_read_only()
            || self
                .startup_hints
                .inherited_turn_authority
                .as_ref()
                .is_some_and(|authority| authority.prompt_mode.is_read_only())
    }

    /// A prompt rewrite cannot remove the previous profile's registered tools.
    /// Rebuild at automation boundaries and propagate a busy/build failure to ACP
    /// before advertising the new mode. The existing rebuild preserves history
    /// and reconnects the same authenticated session MCP configuration.
    pub(super) async fn apply_session_mode(
        &self,
        mode: acp::SessionModeId,
    ) -> Result<(), acp::Error> {
        if self.client_identifier.as_deref() == Some("echo-agent-desktop")
            && matches!(mode.0.as_ref(), "default" | "ask" | "plan")
            && self.state.lock().await.running_task.is_some()
        {
            // The selector applies to the next user turn. Keep the actual
            // running turn's immutable authority and plan tracker untouched.
            *self.current_prompt_mode.lock() = prompt_mode_from_session_mode_id(&mode);
            return Ok(());
        }
        let active = self.active_agent_type.lock().clone();
        let was_automation = matches!(active.as_deref(), Some("computer-use" | "browser-use"));
        let entering_automation = matches!(mode.0.as_ref(), "computer_use" | "browser_use");
        let return_definition = (!was_automation && entering_automation)
            .then(|| self.agent.borrow().definition().clone());
        let next = match mode.0.as_ref() {
            "computer_use" => Some(AgentDefinition::computer_use()),
            "browser_use" => Some(AgentDefinition::browser_use()),
            "default" | "ask" | "plan"
                if matches!(active.as_deref(), Some("computer-use" | "browser-use")) =>
            {
                Some(
                    self.rebuild_spec
                        .regular_mode_definition
                        .lock()
                        .clone()
                        .unwrap_or_else(AgentDefinition::default_echo_agent_build),
                )
            }
            _ => None,
        };
        if let Some(definition) = next {
            if active.as_deref() != Some(definition.name.as_str()) {
                self.handle_rebuild_agent_for_definition(definition).await?;
            }
        }
        if let Some(definition) = return_definition {
            *self.rebuild_spec.regular_mode_definition.lock() = Some(definition);
        }
        let leaving_automation =
            was_automation && matches!(mode.0.as_ref(), "default" | "ask" | "plan");
        self.handle_session_mode(mode).await;
        if leaving_automation {
            self.rebuild_spec.regular_mode_definition.lock().take();
        }
        Ok(())
    }
    pub(super) fn apply_prompt_modes_to_snapshot(&self, snapshot: &mut TurnDeltaSnapshot) {
        snapshot.start_prompt_mode = Some(self.turn_start_prompt_mode.lock().to_string());
        snapshot.end_prompt_mode = Some(self.turn_prompt_mode.lock().to_string());
    }
    /// `false` twin: this template integration is not compiled into this
    /// build, so no session runs it. Keeps ungated call sites compiling in
    /// both configurations.
    pub(super) fn is_cursor_harness(&self) -> bool {
        false
    }
    pub(super) async fn handle_session_mode(&self, session_mode_id: acp::SessionModeId) {
        use echo_agent_tools::types::SessionMode;
        let prompt_mode = prompt_mode_from_session_mode_id(&session_mode_id);
        *self.current_prompt_mode.lock() = prompt_mode;
        let mode = SessionMode::from_id(session_mode_id.0.as_ref());
        if mode.is_plan() {
            let entered = self.plan_mode.lock().enter_pending();
            if entered {
                self.persist_plan_mode_state();
                self.enqueue_current_mode_update(acp::SessionModeId::new(
                    SessionMode::Plan.as_id(),
                ));
            }
            tracing::info!(
                session_id = %self.session_info.id.0,
                entered,
                "Plan mode toggled ON (Pending)"
            );
            let turn_in_flight = self.state.lock().await.running_task.is_some();
            if entered && turn_in_flight {
                self.activate_plan_mode_mid_turn().await;
            }
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::PlanModeToggled {
                    enabled: true,
                    trigger: echo_agent_telemetry::events::PlanModeTrigger::User,
                    turn_in_flight,
                    was_previously_active: !entered,
                },
            );
            if entered {
                tracing::info_span!(
                    "session.permission_mode_changed",
                    from_mode =
                        super::telemetry::permission_mode_label(self.permissions.is_yolo_mode()),
                    to_mode = "plan",
                    trigger = "user",
                    enabled = true,
                )
                .in_scope(|| {});
            }
            return;
        }
        let was_plan = {
            let tracker = self.plan_mode.lock();
            tracker.state() != crate::session::plan_mode::PlanModeState::Inactive
        };
        if was_plan {
            let turn_in_flight = self.state.lock().await.running_task.is_some();
            self.plan_mode.lock().user_exit(turn_in_flight);
            self.persist_plan_mode_state();
            tracing::info!(
                session_id = %self.session_info.id.0,
                new_mode = %session_mode_id.0,
                turn_in_flight,
                "Plan mode toggled OFF"
            );
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::PlanModeToggled {
                    enabled: false,
                    trigger: echo_agent_telemetry::events::PlanModeTrigger::User,
                    turn_in_flight,
                    was_previously_active: true,
                },
            );
            tracing::info_span!(
                "session.permission_mode_changed",
                from_mode = "plan",
                to_mode = %session_mode_id.0,
                trigger = "user",
                enabled = false,
            )
            .in_scope(|| {});
        }
        let agent_def = match session_mode_id.0.as_ref() {
            "browser_use" => Some(AgentDefinition::browser_use()),
            "computer_use" => Some(AgentDefinition::computer_use()),
            "default" | "ask" => Some(
                self.rebuild_spec
                    .regular_mode_definition
                    .lock()
                    .clone()
                    .unwrap_or_else(AgentDefinition::default_echo_agent_build),
            ),
            name => {
                let cwd = self.tool_context.cwd.as_path();
                echo_agent_core::discovery::by_name_in_cwd(name, cwd)
            }
        };
        if let Some(ref def) = agent_def {
            tracing::info!(
                session_id = %self.session_info.id.0,
                agent_name = %def.name,
                agent_scope = %def.scope,
                prompt_mode = ?def.prompt_mode,
                has_completion_req = def.completion_requirement.is_some(),
                tool_configs = def.tool_config.tools.len(),
                "Resolved AgentDefinition for session mode"
            );
            self.agent
                .borrow()
                .update_policies_from_definition(def)
                .await;
            *self.active_agent_type.lock() = Some(def.name.clone());
        }
        if let Some(ref def) = agent_def {
            let new_prompt = self.agent.borrow().render_prompt_for_definition(def).await;
            let mut conversation = self.chat_state_handle.get_conversation().await;
            for item in conversation.iter_mut() {
                if let ConversationItem::System(sys) = item {
                    sys.content = std::sync::Arc::<str>::from(new_prompt);
                    break;
                }
            }
            self.chat_state_handle.replace_conversation(conversation);
        }
        self.enqueue_current_mode_update(session_mode_id);
    }
    /// Settle the mode a turn runs in, applying the prompt's declaration when
    /// it made one.
    ///
    /// Only a real user turn declares a mode. A synthetic turn — a background
    /// task wake, a goal summary, a notification drain — is constructed
    /// internally with a placeholder `PromptMode::Agent` that reads as "the
    /// user asked for agent mode", so reconciling one ends plan mode just by
    /// waking the session: a background task finishing while you were planning
    /// was enough to do it. Those turns inherit the session's mode instead.
    ///
    /// Returns the resolved mode rather than echoing the argument, so a
    /// synthetic turn is also *recorded* under the mode it really ran in.
    pub(super) fn resolve_turn_prompt_mode(
        &self,
        origin: &crate::session::PromptOrigin,
        declared: PromptMode,
    ) -> PromptMode {
        if !origin.is_synthetic() {
            self.reconcile_plan_mode_with_prompt(declared);
        }
        *self.current_prompt_mode.lock()
    }
    /// Bring the plan-mode tracker into agreement with the prompt's mode.
    ///
    /// Mirrors `handle_session_mode` but driven from `_meta.mode` on the
    /// prompt — the only signal the client sends. Both transitions are
    /// idempotent, so `set_mode`-driven flows are unaffected.
    ///
    /// Like `handle_session_mode`, a real transition here emits a
    /// `CurrentModeUpdate`. Without it a client that carries its mode on the
    /// prompt could enter or leave plan mode with no signal at all — and since
    /// the same line is what lands in `updates.jsonl`, a later replay could not
    /// recover the mode either.
    pub(super) fn reconcile_plan_mode_with_prompt(&self, prompt_mode: PromptMode) {
        use crate::session::plan_mode::PlanModeState;
        *self.current_prompt_mode.lock() = prompt_mode;
        match prompt_mode {
            PromptMode::Plan => {
                let entered = self.plan_mode.lock().enter_pending();
                if entered {
                    self.persist_plan_mode_state();
                    self.enqueue_current_mode_update(session_mode_id_from_prompt_mode(prompt_mode));
                }
            }
            PromptMode::Agent | PromptMode::Ask => {
                let was_plan = {
                    let tracker = self.plan_mode.lock();
                    tracker.state() != PlanModeState::Inactive
                };
                if was_plan {
                    self.plan_mode.lock().user_exit(false);
                    self.persist_plan_mode_state();
                    self.enqueue_current_mode_update(session_mode_id_from_prompt_mode(prompt_mode));
                }
            }
        }
        if self.client_identifier.as_deref() == Some("echo-agent-desktop")
            || self.startup_hints.inherited_turn_authority.is_some()
        {
            // Each admitted prompt has durable authority even for Ask↔Agent
            // transitions that do not enter/leave the plan tracker. This rail
            // persists to updates.jsonl and is replayed on renderer/restart.
            self.enqueue_current_mode_update(session_mode_id_from_prompt_mode(prompt_mode));
        }
    }
    /// Inject plan mode system-reminders into the conversation.
    ///
    /// Called once per turn from `handle_prompt()`, before the user's actual
    /// message is pushed. Handles three mutually-ordered cases:
    ///
    /// 1. **Pending → Active**: First prompt after user toggled plan mode on.
    ///    Injects the full (or reentry) reminder and transitions to Active.
    /// 2. **Already Active**: Subsequent prompts while plan mode is on.
    ///    Injects an alternating full/sparse per-turn reminder.
    /// 3. **Exit reminder**: One-shot reminder after plan mode was exited.
    ///    Injected once, then the flag is cleared.
    ///
    /// All reminders are pushed as `<system-reminder>`-wrapped user messages
    /// so the model sees them in the same turn as the user's prompt.
    /// Tool names are resolved at render time via `TemplateRenderer`.
    pub(super) async fn inject_plan_mode_reminders(&self) {
        use crate::session::plan_mode::{
            PlanModeState, plan_mode_exit_reminder_template, plan_mode_reminder_full_template,
            plan_mode_reminder_sparse_template,
        };
        let use_cursor_reminders = self.is_cursor_harness();
        let push_reminder = |this: &Self, content: &str| {
            this.push_system_reminder_with_tag(content, this.reminder_wrapper_tag());
        };
        let mut injected_this_turn = false;
        let activation = {
            let tracker = self.plan_mode.lock();
            (tracker.state() == PlanModeState::Pending)
                .then(|| (tracker.is_reentry(), tracker.plan_file_path().to_path_buf()))
        };
        if let Some((is_reentry, plan_path)) = activation {
            self.plan_mode.lock().activate();
            self.persist_plan_mode_state();
            let plan_has_content =
                crate::session::plan_mode::plan_file_has_content(&plan_path).await;
            let template = self.plan_activation_template(is_reentry);
            if let Some(rendered) = self
                .render_plan_template(template, &plan_path, plan_has_content)
                .await
            {
                push_reminder(self, &rendered);
                injected_this_turn = true;
                self.plan_mode.lock().record_reminder_injected();
                self.persist_plan_mode_state();
                tracing::info!(
                    session_id = %self.session_info.id.0,
                    is_reentry,
                    uses_template_reminders = use_cursor_reminders,
                    "Plan mode activated: injected system-reminder"
                );
            }
        }
        if !injected_this_turn {
            let per_turn = {
                let tracker = self.plan_mode.lock();
                tracker.is_active().then(|| {
                    (
                        tracker.should_use_full_reminder(),
                        tracker.plan_file_path().to_path_buf(),
                    )
                })
            };
            if let Some((use_full, plan_path)) = per_turn {
                let plan_has_content =
                    crate::session::plan_mode::plan_file_has_content(&plan_path).await;
                let template = if use_full {
                    plan_mode_reminder_full_template()
                } else {
                    plan_mode_reminder_sparse_template()
                };
                if let Some(rendered) = self
                    .render_plan_template(template, &plan_path, plan_has_content)
                    .await
                {
                    push_reminder(self, &rendered);
                    self.plan_mode.lock().record_reminder_injected();
                    self.persist_plan_mode_state();
                }
            }
        }
        if self.plan_mode.lock().has_pending_exit_reminder() {
            let plan_path = self.plan_mode.lock().plan_file_path().to_path_buf();
            let template = plan_mode_exit_reminder_template();
            if let Some(rendered) = self.render_plan_template(template, &plan_path, false).await {
                push_reminder(self, &rendered);
            }
            self.plan_mode.lock().clear_pending_exit_reminder();
            self.persist_plan_mode_state();
        }
    }
    /// Activate plan mode for a turn that is already running.
    ///
    /// Mid-turn counterpart of `inject_plan_mode_reminders` case 1: the user
    /// toggled plan mode ON (Shift+Tab) while the model was thinking, so the
    /// tracker sits in `Pending` and the running turn would otherwise proceed
    /// without any plan-mode instruction. Activate immediately (so
    /// `is_active()` tool gating applies to subsequent calls) and buffer the
    /// activation reminder on the tracker; `flush_pending_skill_reminders`
    /// delivers it at the running turn's next safe point (loop top / after
    /// each tool batch) — or, if the turn ends first, the cancel/idle flush
    /// lands it for the next turn. Buffering (vs a direct conversation push)
    /// keeps the in-flight batch's tool_result blocks adjacent, and lets a
    /// toggle-off withdraw an undelivered reminder (`user_exit`).
    ///
    /// No-op unless the tracker is `Pending`: `enter_pending`'s
    /// `ExitPending → Active` re-entry needs no reminder (the model already
    /// has plan-mode context and no exit reminder was injected yet).
    ///
    /// A failed template render still activates (without a buffer), keeping
    /// gating in lockstep with the turn-start path.
    pub(super) async fn activate_plan_mode_mid_turn(&self) {
        use crate::session::plan_mode::PlanModeState;
        let activation = {
            let tracker = self.plan_mode.lock();
            (tracker.state() == PlanModeState::Pending)
                .then(|| (tracker.is_reentry(), tracker.plan_file_path().to_path_buf()))
        };
        let Some((is_reentry, plan_path)) = activation else {
            return;
        };
        let plan_has_content = crate::session::plan_mode::plan_file_has_content(&plan_path).await;
        let template = self.plan_activation_template(is_reentry);
        let rendered = self
            .render_plan_template(template, &plan_path, plan_has_content)
            .await;
        let tag = self.reminder_wrapper_tag();
        let buffered = rendered.is_some();
        let activated = match rendered {
            Some(rendered) => self
                .plan_mode
                .lock()
                .activate_mid_turn(format!("<{tag}>\n{rendered}\n</{tag}>")),
            None => {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    "Mid-turn plan activation: reminder render failed; \
                     activating without a buffered reminder"
                );
                self.plan_mode.lock().activate()
            }
        };
        if !activated {
            return;
        }
        self.persist_plan_mode_state();
        tracing::info!(
            session_id = %self.session_info.id.0,
            is_reentry,
            buffered,
            "Plan mode activated mid-turn"
        );
    }
    /// The activation reminder template for the active template (no
    /// first-entry/reentry distinction), or echoagent's reentry/full variant.
    /// Shared by turn-start injection (`inject_plan_mode_reminders` case 1)
    /// and the mid-turn toggle (`activate_plan_mode_mid_turn`).
    fn plan_activation_template(&self, is_reentry: bool) -> &'static str {
        use crate::session::plan_mode::{
            plan_mode_reentry_reminder_template, plan_mode_reminder_full_template,
        };
        if is_reentry {
            plan_mode_reentry_reminder_template()
        } else {
            plan_mode_reminder_full_template()
        }
    }
    /// Render a plan mode template via the tool bridge's `TemplateRenderer`.
    ///
    /// Passes `plan_path` and `plan_has_content` as extra context alongside the
    /// registry's `tools.by_kind.*` mappings.
    pub(super) async fn render_plan_template(
        &self,
        template: &str,
        plan_path: &std::path::Path,
        plan_has_content: bool,
    ) -> Option<String> {
        let extra = serde_json::json!({
            "plan_path": plan_path.display().to_string(),
            "plan_has_content": plan_has_content,
        });
        self.agent
            .borrow()
            .tool_bridge()
            .render_prompt(template, &extra)
            .await
    }
    /// Persist the current plan mode state to disk.
    ///
    /// Called after every state transition so plan mode survives
    /// session reload/resume/reconnect.
    pub(super) fn persist_plan_mode_state(&self) {
        let snapshot = self.plan_mode.lock().snapshot();
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::PlanModeState(snapshot));
    }
}

#[cfg(test)]
mod readonly_capability_tests {
    use super::*;
    use echo_agent_tools::types::tool::ToolKind;
    #[test]
    fn readonly_capabilities_exclude_execution_dispatch_and_mode_escalation() {
        for kind in [
            ToolKind::Execute,
            ToolKind::Write,
            ToolKind::Edit,
            ToolKind::UseTool,
            ToolKind::SearchTool,
            ToolKind::Skill,
            ToolKind::Workflow,
            ToolKind::EnterPlan,
            ToolKind::ExitPlan,
            ToolKind::Other,
        ] {
            assert!(!readonly_tool_kind_allowed(Some(kind)), "{kind:?}");
        }
        assert!(!readonly_tool_kind_allowed(None));
        assert!(readonly_tool_kind_allowed(Some(ToolKind::Read)));
        assert!(readonly_tool_kind_allowed(Some(ToolKind::Search)));
        assert!(readonly_tool_kind_allowed(Some(ToolKind::Task)));
    }
    #[test]
    fn dynamic_tool_cannot_claim_readonly_via_its_name_or_arguments() {
        assert!(!readonly_tool_input_allowed(&ToolInput::Dynamic(
            serde_json::json!({
                "name": "read_file", "readOnlyHint": true
            })
        )));
        let input = ToolInput::Bash(
            echo_agent_tools::implementations::echo_agent_build::bash::BashToolInput {
                command: "cat README.md".into(),
                timeout: None,
                description: "read".into(),
                is_background: false,
            },
        );
        assert!(!readonly_tool_input_allowed(&input));
    }
    #[test]
    fn readonly_child_cannot_request_worktree_resume_or_other_cwd() {
        let mut task: echo_agent_tool_types::TaskToolInput =
            serde_json::from_value(serde_json::json!({"prompt":"review","description":"review"}))
                .unwrap();
        assert!(readonly_tool_input_allowed(&ToolInput::Task(task.clone())));
        task.cwd = Some("/other-workspace".into());
        assert!(!readonly_tool_input_allowed(&ToolInput::Task(task.clone())));
        task.cwd = None;
        task.resume_from = Some("old-child".into());
        assert!(!readonly_tool_input_allowed(&ToolInput::Task(task.clone())));
        task.resume_from = None;
        task.isolation = Some(echo_agent_tool_types::SubagentIsolationMode::Worktree);
        assert!(!readonly_tool_input_allowed(&ToolInput::Task(task)));
    }
}
