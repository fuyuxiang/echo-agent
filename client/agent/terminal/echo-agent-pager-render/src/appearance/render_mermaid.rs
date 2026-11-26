//! The `render_mermaid` user setting (`auto | on | off`).
//!
//! Fenced ` ```mermaid ` blocks are rendered inline as Unicode box-drawing art
//! by the markdown renderer. This setting controls the full-fidelity affordance
//! row layered beneath that art: `auto`/`on` add the clickable row
//! (`◇ mermaid [Open Image] [Copy Image Path] [Copy Source]`); `off` shows the
//! inline art alone. The PNG render engine is always compiled in, and the PNG is
//! never drawn as an inline image (it opens in the OS viewer), so the treatment
//! is identical in every terminal.

/// User preference for rendering Mermaid diagrams.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RenderMermaid {
    /// Show the diagram's clickable affordance row. The default.
    #[default]
    Auto,
    /// Explicit opt-in; behaves identically to [`Auto`](Self::Auto) (terminal
    /// capability is never consulted — the affordance row is text + hit-rects).
    On,
    /// Show the inline diagram art alone, without the affordance row.
    Off,
}

impl RenderMermaid {
    /// Canonical persisted string (matches the settings-registry choices).
    pub fn as_canonical(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// Parse a canonical string, returning `None` for unrecognized input so
    /// callers can fall back to the default rather than guess.
    pub fn from_canonical(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_round_trips() {
        for kind in [RenderMermaid::Auto, RenderMermaid::On, RenderMermaid::Off] {
            assert_eq!(
                RenderMermaid::from_canonical(kind.as_canonical()),
                Some(kind)
            );
        }
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(RenderMermaid::default(), RenderMermaid::Auto);
        assert_eq!(RenderMermaid::default().as_canonical(), "auto");
    }

    #[test]
    fn unknown_canonical_is_none() {
        assert_eq!(RenderMermaid::from_canonical("yes"), None);
        assert_eq!(RenderMermaid::from_canonical(""), None);
        assert_eq!(RenderMermaid::from_canonical("Auto"), None);
    }
}
