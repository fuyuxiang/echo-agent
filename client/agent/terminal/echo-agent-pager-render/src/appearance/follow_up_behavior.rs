//! The `follow_up_behavior` user setting (`queue` | `steer`).
//!
//! Controls mid-turn follow-ups after they land on the server queue.
//! **queue** (default) waits for the current turn to finish. **steer**
//! promotes them into a mid-turn interjection at the next safe gap (after a
//! tool batch, at the next model step, or before turn complete).

/// How mid-turn follow-ups join the running session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FollowUpBehavior {
    /// Wait for turn end (classic queue).
    #[default]
    Queue,
    /// Interject at the next tool/model safe point (Codex "Steer").
    Steer,
}

impl FollowUpBehavior {
    /// Canonical persisted string (matches the settings-registry choices).
    pub fn as_canonical(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }

    /// Parse a canonical string; `None` for junk so callers fall back to default.
    pub fn from_canonical(value: &str) -> Option<Self> {
        match value {
            "queue" => Some(Self::Queue),
            "steer" => Some(Self::Steer),
            _ => None,
        }
    }

    /// True when follow-ups should promote as mid-turn interjections.
    pub fn is_steer(self) -> bool {
        matches!(self, Self::Steer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_round_trips() {
        for kind in [FollowUpBehavior::Queue, FollowUpBehavior::Steer] {
            assert_eq!(
                FollowUpBehavior::from_canonical(kind.as_canonical()),
                Some(kind)
            );
        }
    }

    #[test]
    fn default_is_queue() {
        assert_eq!(FollowUpBehavior::default(), FollowUpBehavior::Queue);
        assert_eq!(FollowUpBehavior::default().as_canonical(), "queue");
    }

    #[test]
    fn unknown_canonical_is_none() {
        assert_eq!(FollowUpBehavior::from_canonical("yes"), None);
        assert_eq!(FollowUpBehavior::from_canonical(""), None);
        assert_eq!(FollowUpBehavior::from_canonical("Queue"), None);
        assert_eq!(FollowUpBehavior::from_canonical("interject"), None);
    }
}
