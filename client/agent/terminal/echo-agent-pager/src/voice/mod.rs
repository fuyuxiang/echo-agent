//! Voice input: STT pipeline integration and prompt-box dictation.
//!
//! Layering (pager-owned):
//! - **Voice gate** — GA default **on**. Remote `voice_mode_enabled: false` is
//!   a kill switch (every voice surface unavailable and silent — no toast).
//!   Absent remote falls through to on. `ECHO_AGENT_VOICE_MODE` overrides for local
//!   dev (env > remote > default on). Free/X Basic still get EchoAgent upsell
//!   via tier gates (not this flag).
//! - **Session mode** (`voice_ui_active`) — this CLI run only; shows the mic.
//! - **Capture chord** — `/voice` or `Ctrl+Space` start dictation (Esc/Enter
//!   stop). `Ctrl+Space` decodes identically on every terminal, so the
//!   cheatsheet shows it whenever voice is enabled.
//! - **Hold-to-talk** — `Ctrl+Space`: hold to record, release to stop, on
//!   terminals that report key releases (Kitty protocol); elsewhere the same
//!   chord toggles instead (press starts, press again stops). Handled in
//!   `app::event_loop`.
//!
//! Finals append to the recording target's prompt — the agent prompt or the
//! dashboard's dispatch (new-agent) input, captured at start via
//! [`crate::app::app_view::VoiceTarget`] — while capture stays open across
//! speech pauses. The user always submits with Enter; nothing is auto-sent.
//! Submit promotes any remaining interim into the bound prompt, then hard-resets.

mod auth;
mod handle;

pub use auth::build_voice_auth;
pub use handle::handle_voice_event;
pub(crate) use handle::{combine_prompt_with_voice_text, commit_interim_into_prompt};
// Hidden `__mic-capture` helper intercept (macOS out-of-process capture),
// re-exported for the composition-root binary, which links the pager library
// rather than the voice crate. Called at the very top of `main`.
pub use echo_agent_voice::maybe_run_capture_subprocess;
