use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseKind {
    User,
    BackOff,
    NoProgress,
    Verification,
    Infra,
}

impl PauseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::BackOff => "back_off",
            Self::NoProgress => "no_progress",
            Self::Verification => "verification",
            Self::Infra => "infra",
        }
    }
}

impl std::str::FromStr for PauseKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "back_off" | "backoff" => Ok(Self::BackOff),
            "no_progress" => Ok(Self::NoProgress),
            "verification" | "blocked" => Ok(Self::Verification),
            "infra" => Ok(Self::Infra),
            other => Err(format!("unknown pause kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkflowOutcome {
    Completed { result: serde_json::Value },
    Paused { kind: PauseKind, message: String },
    BudgetExceeded { message: String },
    Cancelled,
    Failed { error: String },
}
