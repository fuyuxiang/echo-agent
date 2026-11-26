//! The `scroll_mode` user setting (`auto` | `wheel` | `trackpad`).
//!
//! Wheel-vs-trackpad detection is heuristic (terminal scroll events carry no
//! magnitude), so this setting lets a user force one classification when the
//! heuristic is wrong for their setup. The pager's input layer maps it onto
//! `ScrollInputMode` when building its scroll config; this crate only owns
//! the persisted value type and its cache.

/// Scroll input classification preference: auto-detect or force one kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ScrollMode {
    /// Detect wheel vs trackpad per stream from event timing. Default.
    #[default]
    Auto,
    /// Always treat scroll input as a mouse wheel (fixed lines per tick).
    Wheel,
    /// Always treat scroll input as a trackpad (fractional accumulation).
    Trackpad,
}

impl ScrollMode {
    /// Canonical persisted string (matches the settings-registry choices).
    pub const fn as_canonical(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Wheel => "wheel",
            Self::Trackpad => "trackpad",
        }
    }

    /// Parse a canonical string, returning `None` for unrecognized input.
    pub fn from_canonical(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "wheel" => Some(Self::Wheel),
            "trackpad" => Some(Self::Trackpad),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_round_trips() {
        for mode in [ScrollMode::Auto, ScrollMode::Wheel, ScrollMode::Trackpad] {
            assert_eq!(ScrollMode::from_canonical(mode.as_canonical()), Some(mode));
        }
    }

    #[test]
    fn junk_and_case_variants_are_rejected() {
        // Strict parse: unknown disk/env values must fall back to the default
        // at the caller (cache seed), never panic or mis-map.
        for junk in ["", "Auto", "WHEEL", "track pad", "mouse", "1"] {
            assert_eq!(ScrollMode::from_canonical(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(ScrollMode::default(), ScrollMode::Auto);
        assert_eq!(ScrollMode::default().as_canonical(), "auto");
    }
}
