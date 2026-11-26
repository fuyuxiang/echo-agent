//! External-OTEL emission gate.
//!
//! Single owner of the fail-closed gate that decides whether customer-owned
//! OTEL telemetry may ship, over the process-global flag in
//! [`echo_agent_telemetry::external`]:
//!
//! 1. Startup (no leader instance yet): [`suppress`] closes the gate before
//!    telemetry init; [`open_at_startup`] re-opens it when nothing will deliver
//!    a fleet policy to this process ([`should_open_at_startup`]).
//! 2. Post-auth/refresh (per-leader): [`OtelGate::resolve`] drives the gate
//!    from the [`SettingsFetch`] outcome for the still-live identity.

use std::time::Duration;

use crate::remote::SettingsFetch;
use crate::util::config::RemoteSettings;

pub(crate) const SETTINGS_GATE_MAX_WAIT: Duration = crate::http::SETTINGS_REAPPLY_TIMEOUT;

/// Closes the gate. Process-global and idempotent; callable before any
/// `AgentConfig` exists.
pub(crate) fn suppress() {
    echo_agent_telemetry::external::set_settings_gate_max_wait(SETTINGS_GATE_MAX_WAIT);
    echo_agent_telemetry::external::suppress_external_otel_until_settings();
}

/// Whether an EchoAgent fleet policy can govern this process at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyChannel {
    Applies,
    Unavailable(NoPolicy),
}

impl PolicyChannel {
    pub(crate) fn is_unavailable(self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// Why no fleet policy can reach this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoPolicy {
    RemoteFetchDisabled,
    ProxyRepointed,
}

pub(crate) fn policy_channel(
    remote_fetch_enabled: bool,
    proxy_is_echo_agent: bool,
) -> PolicyChannel {
    if !remote_fetch_enabled {
        return PolicyChannel::Unavailable(NoPolicy::RemoteFetchDisabled);
    }
    if !proxy_is_echo_agent {
        return PolicyChannel::Unavailable(NoPolicy::ProxyRepointed);
    }
    PolicyChannel::Applies
}

/// [`policy_channel`] resolved against live config for the proxy actually in
/// use.
pub(crate) fn policy_channel_for(proxy_url: &str) -> PolicyChannel {
    policy_channel(
        crate::util::config::resolve_remote_fetch_enabled(),
        crate::util::is_cli_chat_proxy_url(proxy_url),
    )
}

/// [`policy_channel_for`] against the effective config, for startup call sites
/// that run before an `AgentConfig` exists.
pub(crate) fn resolved_policy_channel() -> PolicyChannel {
    policy_channel_for(&crate::agent::config::EndpointsConfig::from_effective_config().proxy_url())
}

/// Inputs to [`should_open_at_startup`]. Named fields prevent transposed
pub(crate) struct StartupGate {
    pub(crate) channel: PolicyChannel,
    pub(crate) has_session: bool,
    pub(crate) session_pending: bool,
}

/// Returns whether a leader opens the gate at startup.
pub(crate) fn should_open_at_startup(gate: StartupGate) -> bool {
    if gate.channel.is_unavailable() {
        return true;
    }
    !gate.has_session && !gate.session_pending
}

/// Returns whether a session-less startup is about to mint a cloud.echo-agent.invalid session
pub(crate) fn is_session_pending(
    has_session: bool,
    echo_agent_com_config: &crate::auth::EchoAgentComConfig,
) -> bool {
    !has_session
        && (echo_agent_com_config.auth_provider_command.is_some()
            || crate::auth::devbox_login::is_devbox_environment())
}

/// Opens the gate at startup once [`should_open_at_startup`] holds; a later session re-resolves via [`OtelGate::resolve`].
pub(crate) fn open_at_startup() {
    echo_agent_telemetry::external::mark_external_otel_settings_resolved();
}

/// Per-leader memory over the process-global external-OTEL gate: the credential
#[derive(Default)]
pub(crate) struct OtelGate {
    resolved_for: std::cell::RefCell<Option<String>>,
}

impl OtelGate {
    /// Re-closes the gate before fetching a different identity's policy, so a
    /// stale open can't leak across an account switch.
    pub(crate) fn rearm_on_switch(&self, identity: &str, channel: PolicyChannel) {
        if channel.is_unavailable() {
            return;
        }
        if identity.is_empty() || self.resolved_for.borrow().as_deref() != Some(identity) {
            suppress();
        }
    }

    /// Drives the gate from a settings-fetch `outcome` for `identity`. Every
    /// outcome for the live identity is definitive and opens the gate; only the
    /// `Fetched` one carries a policy (and settings) to apply.
    pub(crate) fn resolve(
        &self,
        identity: &str,
        outcome: SettingsFetch,
        live_identity: Option<&str>,
    ) -> Option<RemoteSettings> {
        if live_identity != Some(identity) {
            return None;
        }
        match outcome {
            SettingsFetch::Fetched(settings) => {
                self.apply_and_open(identity, Some(&settings));
                Some(*settings)
            }
            SettingsFetch::Rejected | SettingsFetch::Retry => {
                self.apply_and_open(identity, None);
                None
            }
        }
    }

    /// Applies the tighten-only fleet policy from `settings` (`None` on a `401`), then opens the gate and records `identity` (policy before open).
    fn apply_and_open(&self, identity: &str, settings: Option<&RemoteSettings>) {
        crate::agent::config::apply_external_otel_remote_policy(settings);
        echo_agent_telemetry::external::mark_external_otel_settings_resolved();
        *self.resolved_for.borrow_mut() = Some(identity.to_owned());
    }

    #[cfg(test)]
    pub(crate) fn set_resolved_for(&self, identity: &str) {
        *self.resolved_for.borrow_mut() = Some(identity.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_telemetry::external::{
        is_settings_gate_open, mark_external_otel_settings_resolved,
        suppress_external_otel_until_settings,
    };

    /// Restore the process-global gate open on exit so a closed gate never leaks.
    struct RestoreGate;
    impl Drop for RestoreGate {
        fn drop(&mut self) {
            mark_external_otel_settings_resolved();
        }
    }

    fn fetched() -> SettingsFetch {
        SettingsFetch::Fetched(Box::default())
    }

    #[test]
    fn policy_channel_reports_every_structural_reason() {
        assert_eq!(
            policy_channel(false, true),
            PolicyChannel::Unavailable(NoPolicy::RemoteFetchDisabled),
            "remote_fetch off: the deployment declared it never calls EchoAgent"
        );
        assert_eq!(
            policy_channel(true, false),
            PolicyChannel::Unavailable(NoPolicy::ProxyRepointed),
            "a non-EchoAgent proxy is not governed by EchoAgent fleet policy"
        );
        assert_eq!(
            policy_channel(false, false),
            PolicyChannel::Unavailable(NoPolicy::RemoteFetchDisabled),
            "the explicit config decision is reported ahead of the endpoint"
        );
        assert_eq!(
            policy_channel(true, true),
            PolicyChannel::Applies,
            "EchoAgent proxy + fetches allowed: a policy can arrive, so wait for it"
        );
    }

    #[test]
    fn startup_gate_opens_whenever_no_policy_will_arrive() {
        let opens = |channel, has_session, session_pending| {
            should_open_at_startup(StartupGate {
                channel,
                has_session,
                session_pending,
            })
        };
        let applies = PolicyChannel::Applies;

        for reason in [NoPolicy::RemoteFetchDisabled, NoPolicy::ProxyRepointed] {
            let none = PolicyChannel::Unavailable(reason);
            assert!(
                opens(none, true, false),
                "{reason:?}: no policy can arrive, so a session must not wait"
            );
            assert!(opens(none, false, true), "{reason:?}: nor a pending mint");
        }

        assert!(
            !opens(applies, true, false),
            "a session with a reachable policy waits for it"
        );
        assert!(
            !opens(applies, false, true),
            "a pending mint is a session about to exist; wait for its policy"
        );
        assert!(
            opens(applies, false, false),
            "no session and none pending: nothing will query the channel yet"
        );
    }

    #[test]
    #[serial_test::serial]
    fn resolve_opens_on_every_definitive_outcome_for_the_live_identity() {
        let _restore = RestoreGate;
        let gate = OtelGate::default();

        suppress_external_otel_until_settings();
        assert!(
            gate.resolve("alice", SettingsFetch::Retry, Some("alice"))
                .is_none(),
            "a failed fetch yields no settings"
        );
        assert!(
            is_settings_gate_open(),
            "an exhausted fetch must open the gate rather than mute the stream"
        );

        suppress_external_otel_until_settings();
        assert!(
            gate.resolve("alice", SettingsFetch::Rejected, Some("alice"))
                .is_none()
        );
        assert!(
            is_settings_gate_open(),
            "a rejected credential opens the gate"
        );

        suppress_external_otel_until_settings();
        assert!(gate.resolve("alice", fetched(), Some("alice")).is_some());
        assert!(
            is_settings_gate_open(),
            "a fetched outcome opens for the live identity"
        );
    }

    #[test]
    #[serial_test::serial]
    fn resolve_skips_open_for_a_stale_identity() {
        let _restore = RestoreGate;
        let gate = OtelGate::default();

        suppress_external_otel_until_settings();
        assert!(
            gate.resolve("alice", fetched(), Some("bob")).is_none(),
            "a stale identity must not return settings"
        );
        assert!(
            !is_settings_gate_open(),
            "a stale identity must not open the gate"
        );
    }

    #[test]
    #[serial_test::serial]
    fn rearm_re_closes_for_an_empty_identity() {
        let _restore = RestoreGate;
        let gate = OtelGate::default();

        gate.set_resolved_for("");
        mark_external_otel_settings_resolved();
        gate.rearm_on_switch("", PolicyChannel::Applies);
        assert!(
            !is_settings_gate_open(),
            "an empty identity must always re-close (cannot prove same credential)"
        );
    }

    #[test]
    #[serial_test::serial]
    fn rearm_never_re_closes_when_no_policy_can_arrive() {
        let _restore = RestoreGate;
        let gate = OtelGate::default();

        for reason in [NoPolicy::RemoteFetchDisabled, NoPolicy::ProxyRepointed] {
            mark_external_otel_settings_resolved();
            gate.rearm_on_switch("alice", PolicyChannel::Unavailable(reason));
            assert!(
                is_settings_gate_open(),
                "{reason:?}: re-closing would wait on a policy that cannot arrive"
            );
        }
    }
}
