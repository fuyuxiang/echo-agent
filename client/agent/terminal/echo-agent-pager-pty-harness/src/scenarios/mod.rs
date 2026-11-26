//! Named scenarios that drive content into the pager and measure frame timing.
//!
//! Each scenario is a function `async fn run(&mut PtyHarness, &ContentController)`
//! returning a [`BenchResults`]. Scenarios are dispatched by name via the
//! [`Scenario`] enum for the `pty-bench` CLI and for ad-hoc test usage.

use std::time::Duration;

use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use super::{BenchResults, ContentController, PtyHarness};

pub mod empty_enter_send_now;
pub mod idle_cost;
pub mod large_codeblock;
pub mod mixed_interaction;
pub mod plan_approval_resume;
pub mod resize_storm;
pub mod scroll_stress;
pub mod streaming_render;

/// Enumerates every benchmark scenario that can be dispatched by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// Inject rapid `j` keys against a large pre-rendered response.
    ScrollStress,
    /// Stream a 2000-token response at a controlled rate.
    StreamingRender,
    /// Resize the PTY many times in quick succession; assert no crash.
    ResizeStorm,
    /// Render a large syntax-highlighted code block and scroll through it.
    LargeCodeblock,
    /// After content settles, measure CPU cost while idle.
    IdleCost,
    /// Scroll while streaming — the real-world worst case.
    MixedInteraction,
}

impl Scenario {
    /// Every scenario, in dispatch order.
    pub const ALL: &'static [Scenario] = &[
        Scenario::ScrollStress,
        Scenario::StreamingRender,
        Scenario::ResizeStorm,
        Scenario::LargeCodeblock,
        Scenario::IdleCost,
        Scenario::MixedInteraction,
    ];

    /// Stable slug used in JSON output and baseline files.
    pub fn as_str(self) -> &'static str {
        match self {
            Scenario::ScrollStress => "scroll_stress",
            Scenario::StreamingRender => "streaming_render",
            Scenario::ResizeStorm => "resize_storm",
            Scenario::LargeCodeblock => "large_codeblock",
            Scenario::IdleCost => "idle_cost",
            Scenario::MixedInteraction => "mixed_interaction",
        }
    }

    /// Dispatch to the scenario implementation.
    pub async fn run(
        self,
        harness: &mut PtyHarness,
        content: &ContentController,
    ) -> Result<BenchResults> {
        match self {
            Scenario::ScrollStress => scroll_stress::run(harness, content).await,
            Scenario::StreamingRender => streaming_render::run(harness, content).await,
            Scenario::ResizeStorm => resize_storm::run(harness, content).await,
            Scenario::LargeCodeblock => large_codeblock::run(harness, content).await,
            Scenario::IdleCost => idle_cost::run(harness, content).await,
            Scenario::MixedInteraction => mixed_interaction::run(harness, content).await,
        }
    }
}

/// Wait for the pager to render the initial welcome screen. All scenarios
/// that prompt / stream content rely on the pager being past startup.
pub(crate) async fn wait_for_welcome(harness: &mut PtyHarness) -> Result<()> {
    // Menu label on the normal welcome (and gate menus): capital Q — see
    // `echo-agent-pager` `views/welcome/mod.rs` (`"Quit"` in `render_menu`).
    harness
        .wait_for_text("Quit", Duration::from_secs(15))
        .map_err(|e| anyhow::anyhow!("pager failed to reach welcome screen: {e}"))
}
