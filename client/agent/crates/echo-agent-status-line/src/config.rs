//! `[ui.status_line]`, the half of the contract a user writes.
//!
//! Parsing never fails here. A parse error anywhere in `[ui]` discards the
//! whole table, so a value this module cannot read is recorded as a problem
//! rather than rejected.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedStatusLine<'a> {
    Builtin { items: &'a [StatusLineItem] },
    Command { command: &'a str },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatusLineConfig {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<StatusLineType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<Vec<StatusLineItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    padding: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_interval: Option<u64>,
    #[serde(skip)]
    parse_problem: Option<String>,
    #[serde(skip)]
    unknown_keys: Vec<String>,
}

/// Destructured so a new field is a compile error rather than a silent hole.
impl PartialEq for StatusLineConfig {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            kind,
            command,
            items,
            padding,
            refresh_interval,
            parse_problem: _,
            unknown_keys: _,
        } = self;
        *kind == other.kind
            && *command == other.command
            && *items == other.items
            && *padding == other.padding
            && *refresh_interval == other.refresh_interval
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawStatusLineConfig {
    #[serde(rename = "type")]
    kind: Option<Lenient<String>>,
    command: Option<Lenient<String>>,
    items: Option<Lenient<Vec<Lenient<String>>>>,
    padding: Option<Lenient<u16>>,
    refresh_interval: Option<Lenient<u64>>,
    /// `#[serde(untagged)]` replays the table through a fresh deserializer,
    /// so a typo here is reported through `serde_ignored` rather than dropped.
    #[serde(flatten)]
    unknown: BTreeMap<String, serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Read(T),
    Malformed(serde::de::IgnoredAny),
}

fn lenient<T>(field: &str, value: Option<Lenient<T>>, ignored: &mut Vec<String>) -> Option<T> {
    lenient_element(field, value?, ignored)
}

fn lenient_element<T>(field: &str, value: Lenient<T>, ignored: &mut Vec<String>) -> Option<T> {
    match value {
        Lenient::Read(value) => Some(value),
        Lenient::Malformed(_) => {
            ignored.push(field.to_string());
            None
        }
    }
}

impl<'de> Deserialize<'de> for StatusLineConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Lenient::Read(fields) = Lenient::<RawStatusLineConfig>::deserialize(deserializer)?
        else {
            return Ok(Self {
                parse_problem: Some("[ui.status_line] must be a table".to_string()),
                ..Self::default()
            });
        };

        // Field order below is the order problems are reported in.
        let mut ignored: Vec<String> = Vec::new();
        let mut config = Self {
            kind: lenient("type", fields.kind, &mut ignored).and_then(|text| {
                StatusLineType::parse(&text).or_else(|| {
                    ignored.push(format!("type = \"{text}\""));
                    None
                })
            }),
            command: lenient("command", fields.command, &mut ignored),
            items: lenient("items", fields.items, &mut ignored).map(|entries| {
                let mut parsed = Vec::with_capacity(entries.len());
                for entry in entries {
                    let Some(entry) = lenient_element("items", entry, &mut ignored) else {
                        continue;
                    };
                    match StatusLineItem::parse(&entry) {
                        Some(item) => parsed.push(item),
                        None => ignored.push(format!("items = \"{entry}\"")),
                    }
                }
                parsed
            }),
            padding: lenient("padding", fields.padding, &mut ignored),
            refresh_interval: lenient("refresh_interval", fields.refresh_interval, &mut ignored),
            unknown_keys: fields.unknown.into_keys().collect(),
            parse_problem: None,
        };

        let mut seen = BTreeSet::new();
        ignored.retain(|entry| seen.insert(entry.clone()));
        config.parse_problem = if !ignored.is_empty() {
            Some(format!("[ui.status_line] ignored {}", ignored.join(", ")))
        } else if config.kind.is_none() && config.has_payload() {
            // A payload with no `type` is inert; report it rather than drop it.
            Some("[ui.status_line] needs type = \"builtin\" or \"command\"".to_string())
        } else {
            None
        };
        Ok(config)
    }
}

impl StatusLineConfig {
    const DEFAULT_ITEMS: &'static [StatusLineItem] = &[
        StatusLineItem::Cwd,
        StatusLineItem::Model,
        StatusLineItem::Context,
    ];

    pub const MIN_REFRESH_INTERVAL_SECS: u64 = 1;

    /// Capped: unbounded seconds panic `Instant::now() + interval`.
    pub const MAX_REFRESH_INTERVAL_SECS: u64 = 86_400;

    const MAX_PADDING_PER_SIDE: u16 = 16;

    pub fn declared_kind(&self) -> Option<StatusLineType> {
        self.kind
    }

    pub fn has_custom_items(&self) -> bool {
        self.items.is_some()
    }

    pub fn unknown_keys(&self) -> &[String] {
        &self.unknown_keys
    }

    fn effective_kind(&self) -> StatusLineType {
        self.kind.unwrap_or_default()
    }

    pub fn refresh_interval(&self) -> Option<Duration> {
        let secs = self.refresh_interval?;
        match self.resolve() {
            Some(ResolvedStatusLine::Command { .. }) => Some(Duration::from_secs(secs.clamp(
                Self::MIN_REFRESH_INTERVAL_SECS,
                Self::MAX_REFRESH_INTERVAL_SECS,
            ))),
            Some(ResolvedStatusLine::Builtin { .. }) | None => None,
        }
    }

    pub fn padding(&self) -> u16 {
        self.padding.unwrap_or(0).min(Self::MAX_PADDING_PER_SIDE)
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn has_payload(&self) -> bool {
        let Self {
            kind: _,
            command,
            items,
            padding,
            refresh_interval,
            parse_problem: _,
            unknown_keys: _,
        } = self;
        command.is_some() || items.is_some() || padding.is_some() || refresh_interval.is_some()
    }

    pub fn reserves_a_row(&self) -> bool {
        self.resolve().is_some() || self.problem_to_paint().is_some()
    }

    pub fn resolve(&self) -> Option<ResolvedStatusLine<'_>> {
        match self.effective_kind() {
            StatusLineType::Disabled => None,
            StatusLineType::Builtin => {
                let items = self.effective_items();
                (!items.is_empty()).then_some(ResolvedStatusLine::Builtin { items })
            }
            StatusLineType::Command => self
                .command
                .as_deref()
                .filter(|c| !c.trim().is_empty())
                .map(|command| ResolvedStatusLine::Command { command }),
        }
    }

    pub fn problem(&self) -> Option<&str> {
        if let Some(problem) = &self.parse_problem {
            return Some(problem);
        }
        if self.resolve().is_none() {
            return match self.effective_kind() {
                StatusLineType::Command => {
                    Some("[ui.status_line] type = \"command\" needs command = \"…\"")
                }
                StatusLineType::Builtin => {
                    Some("[ui.status_line] type = \"builtin\" needs at least one item")
                }
                // A stray key under `disabled` stays silent, like a stray
                // `command`: the off switch outranks its neighbours.
                StatusLineType::Disabled => None,
            };
        }
        // A timer under `builtin` schedules nothing, so it is reported rather
        if self.refresh_interval.is_some() && self.kind == Some(StatusLineType::Builtin) {
            return Some("[ui.status_line] refresh_interval needs type = \"command\"");
        }
        None
    }

    /// `None` under `type = "disabled"`, so a typo cannot switch the row back on.
    pub fn problem_to_paint(&self) -> Option<&str> {
        if self.kind == Some(StatusLineType::Disabled) || self.resolve().is_some() {
            return None;
        }
        self.problem()
    }

    fn effective_items(&self) -> &[StatusLineItem] {
        self.items.as_deref().unwrap_or(Self::DEFAULT_ITEMS)
    }

    pub fn changes_during_a_turn(&self) -> bool {
        match self.effective_kind() {
            StatusLineType::Builtin => self
                .effective_items()
                .iter()
                .copied()
                .any(StatusLineItem::varies_mid_turn),
            StatusLineType::Command => true,
            StatusLineType::Disabled => false,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum StatusLineType {
    Builtin,
    Command,
    #[default]
    #[strum(
        to_string = "disabled",
        serialize = "off",
        serialize = "none",
        serialize = "hidden"
    )]
    Disabled,
}

impl StatusLineType {
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    fn parse(text: &str) -> Option<Self> {
        text.trim().parse().ok()
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum StatusLineItem {
    Cwd,
    Model,
    Context,
    Cost,
    TurnTimer,
    SessionName,
}

impl StatusLineItem {
    pub const ALL: &'static [StatusLineItem] = Self::VARIANTS;

    pub const fn varies_mid_turn(self) -> bool {
        match self {
            Self::TurnTimer => true,
            Self::Cwd | Self::Model | Self::Context | Self::Cost | Self::SessionName => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        self.into()
    }

    fn parse(text: &str) -> Option<Self> {
        text.trim().parse().ok()
    }
}

#[path = "config_test_support.rs"]
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
