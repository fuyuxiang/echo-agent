//! Session summary (title) generation lifecycle.
//!
//! Encapsulates the full lifecycle: check if a summary already exists,
//! generate one via the LLM, persist it, sync to remote, update the
//! session registry, and notify the client. The persistence actor just
//! calls [`SummaryGenerator::update`] — all state transitions are internal.

use crate::extensions::notification::{
    SessionNotification, SessionUpdate as EchoAgentSessionUpdate,
};
use crate::sampling::Client as OaiCompatClient;
use crate::session::helpers::session_summary::generate_session_summary;
use crate::session::info::Info;
use crate::session::persistence::PersistenceMsg;
use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use tokio::sync::mpsc;

const INITIAL_TITLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Internal state for the summary generation lifecycle.
enum State {
    /// No summary generated yet. Will attempt on the next [`SummaryGenerator::update`] call.
    Idle,
    /// Summary generation has been attempted (spawned or already on disk). No further work needed.
    Done,
}

/// Dependencies for session title generation and fan-out.
pub(crate) struct SummaryConfig {
    pub(crate) sampling_client: OaiCompatClient,
    pub(crate) model: String,
    /// Channel back to the persistence actor for sequential storage writes.
    /// Weak: a strong sender here would keep the actor's own channel and task alive.
    pub(crate) persistence_tx: mpsc::WeakUnboundedSender<PersistenceMsg>,
}

/// Manages session title generation with explicit lifecycle state.
///
/// Created once per persistence actor. [`update`] is called from the
/// `ContentChunk` handler; [`restore`](Self::restore) recovers its state on load.
/// Internally it transitions
/// through `Idle -> Done`, spawning the LLM call as a background task and
/// routing the result back through the persistence channel for storage.
pub(crate) struct SummaryGenerator {
    state: State,
    config: SummaryConfig,
}

impl SummaryGenerator {
    pub(crate) fn new(config: SummaryConfig) -> Self {
        Self {
            state: State::Idle,
            config,
        }
    }

    /// Generate a session summary from the first content chunk.
    ///
    /// - **Idle**: spawns a bounded background task using this first prompt.
    ///   A textless opening gets a stable fallback instead of adopting a later
    ///   question as its title source.
    /// - **Done**: no-op.
    pub(crate) fn update(&mut self, content: String) {
        match self.state {
            State::Done => {}
            State::Idle => {
                // Transition to Done so subsequent ContentChunk messages
                // don't spawn duplicate title generation tasks.
                self.state = State::Done;

                let sampling_client = self.config.sampling_client.clone();
                let model = self.config.model.clone();
                let persistence_tx = self.config.persistence_tx.clone();

                // Spawn title generation as a background task so the
                // persistence actor can continue processing messages
                // (updates, flushes) without waiting for the LLM call.
                tokio::spawn(async move {
                    let title = if content.trim().is_empty() {
                        crate::session::helpers::session_summary::title_fallback_from_user_text(
                            &content,
                        )
                    } else {
                        tokio::time::timeout(
                            INITIAL_TITLE_TIMEOUT,
                            generate_session_summary(content.clone(), sampling_client, &model),
                        )
                        .await
                        .ok()
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| {
                            crate::session::helpers::session_summary::title_fallback_from_user_text(
                                &content,
                            )
                        })
                    };

                    // Route the result through the persistence channel. The
                    // actor persists it (only if the session has no title yet)
                    // and notifies the client there, so a title rejected for
                    // racing a manual `/rename` never reaches the client.
                    match persistence_tx.upgrade() {
                        Some(tx) => {
                            let _ = tx.send(PersistenceMsg::GeneratedTitle(title));
                        }
                        None => tracing::debug!("session closed before its title was generated"),
                    }
                });
            }
        }
    }

    /// Mark as Done (e.g. when disk already has a summary during load).
    pub(crate) fn mark_done(&mut self) {
        self.state = State::Done;
    }

    /// Recover an interrupted first-title call from the original persisted
    /// prompts. Never let a resumed session's next question become its title.
    pub(crate) fn restore(
        &mut self,
        has_title: bool,
        had_history: bool,
        first_prompt: Option<String>,
    ) {
        if has_title {
            self.mark_done();
        } else if let Some(first_prompt) = first_prompt {
            self.update(first_prompt);
        } else if had_history {
            // History could not be recovered. Preserve the existing display
            // fallback rather than guessing from a new, unrelated follow-up.
            self.mark_done();
        }
    }

    /// Inverse of [`mark_done`]: `/rename --auto` so the next content chunk
    /// regenerates a title through the normal if-absent path.
    pub(crate) fn reset(&mut self) {
        self.state = State::Idle;
    }

    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }
}

/// Notify the client that a session summary is available.
pub(crate) fn notify_client(gateway: &Option<GatewaySender>, info: &Info, title: &str) {
    let Some(gateway) = gateway else {
        return;
    };

    let notification = SessionNotification {
        session_id: info.id.clone(),
        update: EchoAgentSessionUpdate::SessionSummaryGenerated {
            session_summary: title.to_owned(),
        },
        meta: None,
    };
    if let Ok(params) = serde_json::value::to_raw_value(&notification) {
        gateway.forward_fire_and_forget(acp::ExtNotification::new(
            "echo.agent/session_notification",
            params.into(),
        ));
    }

    gateway.forward_fire_and_forget(session_info_update(info.id.clone(), title));
}

pub(crate) fn session_info_update(
    session_id: acp::SessionId,
    title: &str,
) -> acp::SessionNotification {
    // `updatedAt` is omitted, not refreshed: renaming is not activity, and
    // `session/list` sorts on `last_active_at`, which a title write never moves.
    acp::SessionNotification::new(
        session_id,
        acp::SessionUpdate::SessionInfoUpdate(
            acp::SessionInfoUpdate::new().title(title.to_owned()),
        ),
    )
}

/// Manual-rename fan-out: same payload as [`session_info_update`] plus
/// `_meta.echo.agent/titleIsManual`. Old clients ignore the unknown key.
pub(crate) fn session_info_update_manual(
    session_id: acp::SessionId,
    title: &str,
) -> acp::SessionNotification {
    session_info_update(session_id, title).meta(
        crate::extensions::notification::title_is_manual_meta()
            .as_object()
            .cloned(),
    )
}

/// Unpin fan-out: no title (avoid blanking list-driven clients) +
/// `_meta.echo.agent/titleIsManual: false`.
pub(crate) fn session_info_update_unpinned(session_id: acp::SessionId) -> acp::SessionNotification {
    acp::SessionNotification::new(
        session_id,
        acp::SessionUpdate::SessionInfoUpdate(acp::SessionInfoUpdate::new()),
    )
    .meta(
        crate::extensions::notification::title_is_unpinned_meta()
            .as_object()
            .cloned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generator(base_url: String) -> (SummaryGenerator, mpsc::UnboundedReceiver<PersistenceMsg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sampling_client = OaiCompatClient::new(echo_agent_sampler::SamplerConfig {
            base_url,
            api_key: Some("test-only".into()),
            model: "test-model".into(),
            max_retries: Some(0),
            ..Default::default()
        })
        .unwrap();
        let generator = SummaryGenerator::new(SummaryConfig {
            sampling_client,
            model: "test-model".into(),
            persistence_tx: tx.downgrade(),
        });
        // The persistence actor owns the strong sender in production. These
        // focused lifecycle tests retain one for the duration of the receiver.
        tokio::spawn(async move { tx.closed().await });
        (generator, rx)
    }

    async fn next_title(rx: &mut mpsc::UnboundedReceiver<PersistenceMsg>) -> String {
        match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            PersistenceMsg::GeneratedTitle(title) => title,
            other => panic!("expected title, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn first_message_is_the_only_title_source_across_followups() {
        let server = echo_agent_test_support::MockInferenceServer::start()
            .await
            .unwrap();
        server.set_response("首问标题");
        let (mut generator, mut rx) = generator(server.url());
        generator.update("修复登录跳转".into());
        generator.update("北京天气怎么样".into());
        assert_eq!(next_title(&mut rx).await, "首问标题");
        for _ in 0..8 {
            generator.update("再问一个新话题".into());
        }
        assert!(rx.try_recv().is_err());
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let body = requests[0].body.as_ref().unwrap().to_string();
        assert!(body.contains("修复登录跳转"));
        assert!(!body.contains("北京天气"));
    }

    #[tokio::test]
    async fn textless_opening_does_not_adopt_a_later_question() {
        let (mut generator, mut rx) = generator("http://127.0.0.1:9/v1".into());
        generator.update(String::new());
        generator.update("后来的问题".into());
        assert_eq!(next_title(&mut rx).await, "New session");
        assert!(!generator.is_idle());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn injected_context_without_user_text_gets_a_neutral_title() {
        let server = echo_agent_test_support::MockInferenceServer::start()
            .await
            .unwrap();
        let (mut generator, mut rx) = generator(server.url());
        generator.update("<!--EXPERT_PERSONA_BEGIN-->专家身份<!--EXPERT_PERSONA_END-->".into());
        generator.update("后续的天气问题".into());
        assert_eq!(next_title(&mut rx).await, "New session");
        assert!(server.requests().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_title_call_falls_back_to_the_opening_message() {
        let server = echo_agent_test_support::MockInferenceServer::start()
            .await
            .unwrap();
        server.set_chunk_delay(Some(std::time::Duration::from_secs(60)));
        let (mut generator, mut rx) = generator(server.url());
        generator.update("修复登录跳转。保留现有鉴权".into());
        generator.update("后来的问题".into());
        tokio::task::yield_now().await;
        tokio::time::advance(INITIAL_TITLE_TIMEOUT + std::time::Duration::from_secs(1)).await;
        assert_eq!(next_title(&mut rx).await, "修复登录跳转");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn restore_retries_original_prompt_and_preserves_existing_titles() {
        let server = echo_agent_test_support::MockInferenceServer::start()
            .await
            .unwrap();
        server.set_response("恢复首问标题");
        let (mut generator, mut rx) = generator(server.url());
        generator.restore(false, true, Some("最初的问题".into()));
        generator.update("恢复后的追问".into());
        assert_eq!(next_title(&mut rx).await, "恢复首问标题");
        let body = server.requests()[0].body.as_ref().unwrap().to_string();
        assert!(body.contains("最初的问题"));
        assert!(!body.contains("恢复后的追问"));

        let (mut existing, mut rx) = self::generator(server.url());
        existing.restore(true, true, Some("最初的问题".into()));
        existing.update("另一个话题".into());
        assert!(!existing.is_idle());
        assert!(rx.try_recv().is_err());

        let (mut unavailable, mut rx) = self::generator(server.url());
        unavailable.restore(false, true, None);
        unavailable.update("不能冒充首问".into());
        assert!(!unavailable.is_idle());
        assert!(rx.try_recv().is_err());
        assert_eq!(server.requests().len(), 1);

        let (mut fresh, mut rx) = self::generator(server.url());
        fresh.restore(false, false, None);
        assert!(
            fresh.is_idle(),
            "an empty resumed session still needs its first title"
        );
        fresh.update("空会话的第一条问题".into());
        assert_eq!(next_title(&mut rx).await, "恢复首问标题");
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn session_info_update_manual_carries_meta_and_raw_title() {
        let n = session_info_update_manual(acp::SessionId::new("s"), "a &amp; b");
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(
            v["_meta"][crate::extensions::notification::TITLE_IS_MANUAL_META_KEY],
            true
        );
        let title = v
            .pointer("/update/title")
            .or_else(|| v.pointer("/update/sessionInfoUpdate/title"))
            .cloned();
        assert_eq!(title, Some(serde_json::json!("a &amp; b")), "{v}");
    }

    #[test]
    fn session_info_update_unpinned_stamps_false_meta_without_title() {
        let n = session_info_update_unpinned(acp::SessionId::new("s"));
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(
            v["_meta"][crate::extensions::notification::TITLE_IS_MANUAL_META_KEY],
            false
        );
        let title = v
            .pointer("/update/title")
            .or_else(|| v.pointer("/update/sessionInfoUpdate/title"));
        assert!(
            title.is_none(),
            "unpin SessionInfoUpdate must omit title: {v}"
        );
    }

    #[test]
    fn auto_session_info_update_omits_manual_meta() {
        let n = session_info_update(acp::SessionId::new("s"), "Auto");
        let v = serde_json::to_value(&n).unwrap();
        assert!(
            v.get("_meta")
                .and_then(|m| m.get(crate::extensions::notification::TITLE_IS_MANUAL_META_KEY))
                .is_none(),
            "auto-title fan-out must not stamp titleIsManual: {v}"
        );
    }

    #[test]
    fn reset_returns_generator_to_idle() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sampling_client =
            OaiCompatClient::new(echo_agent_sampler::SamplerConfig::default()).unwrap();
        let mut generator = SummaryGenerator::new(SummaryConfig {
            sampling_client,
            model: String::new(),
            persistence_tx: tx.downgrade(),
        });
        assert!(generator.is_idle());
        generator.mark_done();
        assert!(!generator.is_idle());
        generator.reset();
        assert!(generator.is_idle());
        generator.reset();
        assert!(generator.is_idle());
    }
}
