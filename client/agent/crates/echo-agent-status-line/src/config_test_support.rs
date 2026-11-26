//! Test-only helpers, public because the tests that need them are in other
//! crates. Production code must not use this module.

use super::{StatusLineConfig, StatusLineItem, StatusLineType};

pub const WIRE_FIXTURE_JSON: &str = include_str!("../testdata/status_wire.json");

#[derive(Debug, Clone, Default)]
pub struct StatusLineConfigFixture {
    config: StatusLineConfig,
}

impl StatusLineConfigFixture {
    /// A section that named this mode and set nothing else.
    pub fn from_kind(kind: StatusLineType) -> Self {
        Self {
            config: StatusLineConfig {
                kind: Some(kind),
                ..StatusLineConfig::default()
            },
        }
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.config.command = Some(command.into());
        self
    }

    pub fn with_items(mut self, items: Vec<StatusLineItem>) -> Self {
        self.config.items = Some(items);
        self
    }

    pub fn with_refresh_interval(mut self, secs: Option<u64>) -> Self {
        self.config.refresh_interval = secs;
        self
    }

    /// Columns per side as a user would write them. The cap still applies, on
    /// the way back out.
    pub fn with_padding(mut self, padding: u16) -> Self {
        self.config.padding = Some(padding);
        self
    }

    pub fn into_config(self) -> StatusLineConfig {
        self.config
    }
}
