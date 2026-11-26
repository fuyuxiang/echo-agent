//! Feedback API request and response types.
//!
//! These types support the feedback collection system for EchoAgent sessions.
//! The agent (echo-agent-runtime) uses heuristics to determine when to request feedback,
//! and clients submit feedback through these types to the feedback backend.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Enums
// ============================================================================

/// Type of client submitting feedback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    /// Terminal/CLI agent
    #[default]
    Agent,
    /// Terminal UI
    Tui,
    /// Web interface
    Web,
    /// IDE extension (VS Code, JetBrains, etc.)
    Extension,
    /// Remote workspace / hosted agent client (wire value `nebula`).
    Nebula,
    /// Desktop (Electron app)
    Desktop,
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientType::Agent => write!(f, "agent"),
            ClientType::Tui => write!(f, "tui"),
            ClientType::Web => write!(f, "web"),
            ClientType::Extension => write!(f, "extension"),
            ClientType::Nebula => write!(f, "nebula"),
            ClientType::Desktop => write!(f, "desktop"),
        }
    }
}

/// Type of feedback being submitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackType {
    /// Numeric rating only
    #[default]
    Rating,
    /// Free-form text only
    Text,
    /// Both rating and text
    RatingWithText,
    /// Model preference comparison
    ModelPreference,
    /// Bug report
    BugReport,
    /// Feature request
    FeatureRequest,
}

impl std::fmt::Display for FeedbackType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedbackType::Rating => write!(f, "rating"),
            FeedbackType::Text => write!(f, "text"),
            FeedbackType::RatingWithText => write!(f, "rating_with_text"),
            FeedbackType::ModelPreference => write!(f, "model_preference"),
            FeedbackType::BugReport => write!(f, "bug_report"),
            FeedbackType::FeatureRequest => write!(f, "feature_request"),
        }
    }
}

/// Type of rating scale used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RatingType {
    /// Thumbs up/down (-1, 0, 1)
    Thumbs,
    /// Star rating (1-5)
    Stars,
    /// Net Promoter Score (0-10)
    Nps,
}

impl std::fmt::Display for RatingType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RatingType::Thumbs => write!(f, "thumbs"),
            RatingType::Stars => write!(f, "stars"),
            RatingType::Nps => write!(f, "nps"),
        }
    }
}

/// Strength of preference in a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceStrength {
    /// Strongly prefer one over the other
    Strong,
    /// Slightly prefer one over the other
    Slight,
    /// No preference, both equal
    Tie,
    /// Both responses are bad
    BothBad,
    /// Both responses are good
    BothGood,
}

impl std::fmt::Display for PreferenceStrength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreferenceStrength::Strong => write!(f, "strong"),
            PreferenceStrength::Slight => write!(f, "slight"),
            PreferenceStrength::Tie => write!(f, "tie"),
            PreferenceStrength::BothBad => write!(f, "both_bad"),
            PreferenceStrength::BothGood => write!(f, "both_good"),
        }
    }
}

/// Status of a feedback request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRequestStatus {
    /// Request created, not yet delivered to client
    Pending,
    /// Request delivered to client, awaiting response
    Delivered,
    /// User submitted feedback
    Completed,
    /// Request expired before user responded
    Expired,
    /// User dismissed the request without responding
    Dismissed,
}

impl std::fmt::Display for FeedbackRequestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedbackRequestStatus::Pending => write!(f, "pending"),
            FeedbackRequestStatus::Delivered => write!(f, "delivered"),
            FeedbackRequestStatus::Completed => write!(f, "completed"),
            FeedbackRequestStatus::Expired => write!(f, "expired"),
            FeedbackRequestStatus::Dismissed => write!(f, "dismissed"),
        }
    }
}

/// Context type for what the feedback is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextType {
    /// Feedback about a specific message
    Message,
    /// Feedback about the overall session/conversation
    Session,
    /// Feedback about a specific feature
    Feature,
    /// Feedback about tool usage
    ToolUse,
    /// General feedback
    General,
}

impl std::fmt::Display for ContextType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextType::Message => write!(f, "message"),
            ContextType::Session => write!(f, "session"),
            ContextType::Feature => write!(f, "feature"),
            ContextType::ToolUse => write!(f, "tool_use"),
            ContextType::General => write!(f, "general"),
        }
    }
}

/// Type of feedback mode requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackMode {
    /// Thumbs up/down
    Thumbs,
    /// Star rating (1-5)
    Stars,
    /// Free-form text
    Text,
    /// Thumbs up/down with optional text comment
    ThumbsText,
    /// Star rating with optional text comment
    StarsText,
    /// Model comparison
    Comparison,
    /// Multi-question survey
    Survey,
    /// Net Promoter Score (0-10)
    Nps,
    /// NPS with optional text comment
    NpsText,
}

impl std::fmt::Display for FeedbackMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedbackMode::Thumbs => write!(f, "thumbs"),
            FeedbackMode::Stars => write!(f, "stars"),
            FeedbackMode::Text => write!(f, "text"),
            FeedbackMode::ThumbsText => write!(f, "thumbs_text"),
            FeedbackMode::StarsText => write!(f, "stars_text"),
            FeedbackMode::Comparison => write!(f, "comparison"),
            FeedbackMode::Survey => write!(f, "survey"),
            FeedbackMode::Nps => write!(f, "nps"),
            FeedbackMode::NpsText => write!(f, "nps_text"),
        }
    }
}

// ============================================================================
// Request Types
// ============================================================================

/// Allowed `feedback_type` + value-field combinations. Construct submissions
/// via [`FeedbackSubmission::with_content`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FeedbackContent {
    Rating {
        rating_type: RatingType,
        rating_value: i32,
    },
    Text(String),
    RatingWithText {
        rating_type: RatingType,
        rating_value: i32,
        text: String,
    },
}

impl FeedbackContent {
    fn apply_to(self, s: &mut FeedbackSubmission) {
        s.rating_type = None;
        s.rating_value = None;
        s.feedback_text = None;
        match self {
            Self::Rating {
                rating_type,
                rating_value,
            } => {
                s.feedback_type = FeedbackType::Rating;
                s.rating_type = Some(rating_type);
                s.rating_value = Some(rating_value);
            }
            Self::Text(text) => {
                s.feedback_type = FeedbackType::Text;
                s.feedback_text = Some(text);
            }
            Self::RatingWithText {
                rating_type,
                rating_value,
                text,
            } => {
                s.feedback_type = FeedbackType::RatingWithText;
                s.rating_type = Some(rating_type);
                s.rating_value = Some(rating_value);
                s.feedback_text = Some(text);
            }
        }
    }
}

pub const MAX_FEEDBACK_IMAGES: usize = 4;

pub const MAX_FEEDBACK_IMAGE_BYTES: usize = 8 * 1024 * 1024;

pub const MAX_FEEDBACK_IMAGE_TOTAL_BYTES: usize = 16 * 1024 * 1024;

/// The allow-list: only the formats Slack image blocks render inline
/// (notably not webp).
pub fn feedback_image_extension(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackImage {
    /// Base64 (standard alphabet) of the raw image bytes.
    pub data: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

impl FeedbackImage {
    /// Exact, so an image at the cap can't pass the TUI's raw-byte check and
    /// fail here.
    pub fn decoded_len(&self) -> usize {
        let unpadded = self.data.trim_end_matches('=');
        unpadded.len() / 4 * 3
            + match unpadded.len() % 4 {
                2 => 1,
                3 => 2,
                _ => 0,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackImageError {
    TooManyImages { count: usize },
    ImageTooLarge { index: usize },
    TotalTooLarge,
    UnsupportedMimeType { index: usize, mime_type: String },
}

impl std::fmt::Display for FeedbackImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyImages { count } => {
                write!(f, "too many images: {count} > {MAX_FEEDBACK_IMAGES}")
            }
            Self::ImageTooLarge { index } => write!(
                f,
                "image {index} exceeds {MAX_FEEDBACK_IMAGE_BYTES} decoded bytes"
            ),
            Self::TotalTooLarge => write!(
                f,
                "images exceed {MAX_FEEDBACK_IMAGE_TOTAL_BYTES} combined decoded bytes"
            ),
            Self::UnsupportedMimeType { index, mime_type } => {
                write!(f, "image {index} has unsupported media type {mime_type}")
            }
        }
    }
}

impl std::error::Error for FeedbackImageError {}

pub fn validate_feedback_images(images: &[FeedbackImage]) -> Result<(), FeedbackImageError> {
    if images.len() > MAX_FEEDBACK_IMAGES {
        return Err(FeedbackImageError::TooManyImages {
            count: images.len(),
        });
    }
    let mut total = 0usize;
    for (index, image) in images.iter().enumerate() {
        if feedback_image_extension(&image.mime_type).is_none() {
            return Err(FeedbackImageError::UnsupportedMimeType {
                index,
                mime_type: image.mime_type.clone(),
            });
        }
        let decoded = image.decoded_len();
        if decoded > MAX_FEEDBACK_IMAGE_BYTES {
            return Err(FeedbackImageError::ImageTooLarge { index });
        }
        total = total.saturating_add(decoded);
    }
    if total > MAX_FEEDBACK_IMAGE_TOTAL_BYTES {
        return Err(FeedbackImageError::TotalTooLarge);
    }
    Ok(())
}

/// Request body for POST /v1/feedback. Construct via
/// [`FeedbackSubmission::with_content`]; the `Default` impl exists for
/// builder-style construction and test fixtures and does not produce a valid
/// submission on its own (empty `session_id`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackSubmission {
    /// Session ID this feedback is for
    pub session_id: String,

    /// User ID (optional, extracted from auth if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// Type of client submitting feedback
    pub client_type: ClientType,

    /// Type of feedback being submitted
    pub feedback_type: FeedbackType,

    /// Turn number within the session (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_number: Option<i64>,

    /// Rating type (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_type: Option<RatingType>,

    /// Rating value (interpretation depends on rating_type)
    /// - thumbs: -1 (down), 0 (neutral), 1 (up)
    /// - stars: 1-5
    /// - nps: 0-10
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_value: Option<i32>,

    /// Free-form feedback text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_text: Option<String>,

    /// Enforce [`validate_feedback_images`] before sending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<FeedbackImage>,

    /// Feedback categories (e.g., ["accuracy", "speed", "helpfulness"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback_categories: Vec<String>,

    /// Message ID this feedback is about (if context_type is message)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,

    /// Model ID used for the response being rated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,

    /// Server-resolved model ID from the actual chat completion response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model_id: Option<String>,

    /// Checkpoint fingerprint from the inference provider (`system_fingerprint`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_helpers::empty_string_as_none"
    )]
    pub model_fingerprint: Option<String>,

    /// Context type for the feedback
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_type: Option<ContextType>,

    /// Feature name (if context_type is feature)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_name: Option<String>,

    /// Tool name (if context_type is tool_use)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,

    // === Experiment / Comparison Fields ===
    /// Experiment ID (e.g. for routing experiments)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,

    /// Comparison ID (e.g. for routing experiments)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_id: Option<String>,

    /// Preferred model ID in comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_model_id: Option<String>,

    /// Strength of preference
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preference_strength: Option<PreferenceStrength>,

    /// Reasons for preference (e.g., ["more_accurate", "faster", "better_code"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preference_reasons: Vec<String>,

    // === Request Link ===
    /// Feedback request ID (links to feedback_requests table if responding to a request)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    // === Client Metadata ===
    /// Client version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,

    /// Shell (echo-agent-runtime) version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_version: Option<String>,

    /// Extension host (for extension client type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_host: Option<String>,

    /// Additional metadata as JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,

    // === Feedback Context ===
    // Persisted server-side alongside the feedback record.
    /// Last user message at feedback time.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "last_user_turn"
    )]
    pub last_user_message: Option<String>,

    /// Last assistant response at feedback time.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "last_assistant_turn"
    )]
    pub last_assistant_message: Option<String>,

    /// Per-tool call counts for the rated turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_outcomes: Vec<FeedbackToolOutcome>,

    /// Session working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cwd: Option<String>,

    /// Number of compactions in the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_count: Option<i64>,

    /// Context window usage percentage (0–100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_usage: Option<u8>,

    /// Raw context tokens used at feedback time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens_used: Option<u64>,

    /// Raw model context window token limit at feedback time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,

    // === Terminal Context ===
    /// Terminal environment snapshot at feedback time (brand, multiplexer, SSH, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_info: Option<FeedbackTerminalInfo>,

    /// Backend URL linking this feedback to its server-side session log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_log_url: Option<String>,

    /// Client-reported display name; unverified, never used for authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    /// Client-reported email; unverified, never used for authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
}

impl FeedbackSubmission {
    /// Construct from typed content; set optional fields after.
    pub fn with_content(
        session_id: String,
        client_type: ClientType,
        content: FeedbackContent,
    ) -> Self {
        let mut s = Self {
            session_id,
            client_type,
            ..Default::default()
        };
        content.apply_to(&mut s);
        s
    }

    /// Remove session context and model metadata. The author identity is
    /// preserved so the author stays reachable after the strip.
    pub fn strip_metadata(&mut self) {
        self.model_id = None;
        self.resolved_model_id = None;
        self.turn_number = None;
        self.last_user_message = None;
        self.last_assistant_message = None;
        self.tool_outcomes = vec![];
        self.session_cwd = None;
        self.compaction_count = None;
        self.context_window_usage = None;
        self.context_tokens_used = None;
        self.context_window_tokens = None;
        self.metadata = None;
        self.terminal_info = None;
    }

    /// Merge a JSON object into `metadata`, inserting if absent.
    pub fn merge_metadata(&mut self, extra: serde_json::Value) {
        match &mut self.metadata {
            Some(existing) if existing.is_object() => {
                if let (Some(dst), Some(src)) = (existing.as_object_mut(), extra.as_object()) {
                    for (k, v) in src {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
            _ => {
                self.metadata = Some(extra);
            }
        }
    }
}

/// Snapshot of the user's terminal environment at feedback time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackTerminalInfo {
    /// Terminal emulator brand (e.g. "Ghostty", "iTerm2", "Unknown").
    pub brand: String,
    /// Multiplexer wrapping the session (e.g. "tmux", "Zellij", "None detected").
    pub multiplexer: String,
    /// Whether the session is over SSH.
    pub is_ssh: bool,
    /// Whether Byobu is wrapping the session.
    pub is_byobu: bool,
    /// Raw `TERM` environment variable value.
    pub term_var: String,
    /// Terminal version exactly as the emulator reports it, when known — not
    /// always the release number, since Alacritty answers with its
    /// `alacritty_terminal` library version (release 0.15.1 arrives as
    /// "0.25.0"). The source that reported it is deliberately not carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term_version: Option<String>,
    /// tmux server version if inside tmux, otherwise "n/a".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_version: Option<String>,
    /// Hyperlink (OSC 8) support level (e.g. "native", "hostile_parser").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlink_osc8_support: Option<String>,
    /// Active clipboard legs, e.g. "native+osc52" or "native+tmux+osc52".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_route: Option<String>,
    /// Native clipboard tool: "pbcopy", "wl-copy", "xclip", "xsel", "arboard".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_native_tool: Option<String>,
    /// Display server: "wayland", "x11", "quartz", "win32", "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_server: Option<String>,
}

/// Per-tool call/failure counts for a single tool in a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackToolOutcome {
    pub tool_name: String,
    pub calls: u32,
    pub failures: u32,
}

/// Response from submitting feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackResponse {
    /// Unique ID of the created feedback entry
    pub feedback_id: String,
    /// Timestamp when feedback was recorded
    pub created_at: DateTime<Utc>,
}

/// Request body for completing a feedback request (with feedback data).
/// POST /v1/feedback/requests/{request_id}/complete
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct FeedbackRequestCompleteRequest {
    /// The feedback submission data
    #[serde(flatten)]
    pub feedback: FeedbackSubmission,
}

/// Response from completing or dismissing a feedback request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackRequestUpdateResponse {
    /// The request ID that was updated
    pub request_id: String,
    /// New status of the request
    pub status: FeedbackRequestStatus,
    /// Feedback ID (only present if completed with feedback)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_id: Option<String>,
    /// Timestamp of the update
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating a new feedback request.
/// POST /v1/feedback/requests
///
/// When the agent decides to request feedback (based on heuristics), it should
/// call this endpoint to create a feedback record before sending the
/// FeedbackRequest notification to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFeedbackRequestInput {
    /// Unique request ID (UUID v7 generated by client)
    pub request_id: String,

    /// Session ID this request is for
    pub session_id: String,

    /// Type of client making the request
    pub client_type: ClientType,

    /// What kind of feedback to collect
    pub feedback_mode: FeedbackMode,

    /// Custom prompt to show user
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_prompt: Option<String>,

    /// Priority (1-10, higher = more important)
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Which heuristic triggered this request (e.g., "tier1_engagement")
    pub trigger_type: String,

    /// Human-readable reason for the request
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_reason: Option<String>,

    /// Context type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_type: Option<ContextType>,

    /// Specific message IDs to rate (if applicable)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_message_ids: Vec<String>,

    /// When the request expires (optional, defaults to 24 hours)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    /// Experiment ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,

    /// Trigger condition details (JSON with signal values at trigger time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_condition: Option<serde_json::Value>,

    /// Per-turn prompt/request ID from the agent session (req_id).
    /// Distinct from request_id which is the feedback request's own UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
}

fn default_priority() -> i32 {
    5
}

/// Response from creating a feedback request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFeedbackRequestResponse {
    /// The request ID that was created
    pub request_id: String,
    /// Timestamp when the request was created
    pub created_at: DateTime<Utc>,
}

/// Request body for recording a session event.
/// POST /v1/sessions/{session_id}/events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventRequest {
    /// Type of event
    pub event_type: SessionEventType,
    /// Event-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_data: Option<serde_json::Value>,
    /// Timestamp of the event (defaults to now if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Types of session events that can be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventType {
    /// User cancelled a response generation
    Cancellation,
    /// An error occurred
    Error,
    /// Context was compacted
    Compaction,
    /// A tool failed
    ToolFailure,
    /// User requested regeneration
    Regeneration,
    /// Session ended
    SessionEnd,
}

impl std::fmt::Display for SessionEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionEventType::Cancellation => write!(f, "cancellation"),
            SessionEventType::Error => write!(f, "error"),
            SessionEventType::Compaction => write!(f, "compaction"),
            SessionEventType::ToolFailure => write!(f, "tool_failure"),
            SessionEventType::Regeneration => write!(f, "regeneration"),
            SessionEventType::SessionEnd => write!(f, "session_end"),
        }
    }
}

/// Response from recording a session event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventResponse {
    /// Whether the event was recorded successfully
    pub success: bool,
    /// Timestamp when event was recorded
    pub recorded_at: DateTime<Utc>,
}

/// Request body for updating session signals.
/// POST /v1/sessions/{session_id}/signals
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSignalsUpdate {
    /// Client type
    pub client_type: ClientType,

    /// Total turns in the session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_turns: Option<i64>,

    /// Number of user messages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_count: Option<i64>,

    /// Number of assistant messages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_message_count: Option<i64>,

    /// Number of cancellations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation_count: Option<i64>,

    /// Number of consecutive cancellations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consecutive_cancellations: Option<i64>,

    /// Number of errors
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<i64>,

    /// Number of tool failures
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_failure_count: Option<i64>,

    /// Total number of tool calls executed (successful + failed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_count: Option<i64>,

    /// Number of compactions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_count: Option<i64>,

    /// Number of regenerations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regeneration_count: Option<i64>,

    /// Number of edit-and-retry actions (user rewinds and submits a different prompt)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_and_retry_count: Option<i64>,

    /// Number of positive ratings (thumbs-up / stars >= 4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positive_ratings: Option<i64>,

    /// Number of negative ratings (thumbs-down / stars <= 2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_ratings: Option<i64>,

    /// Number of long pauses between turns (idle > 60s)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_pauses_count: Option<i64>,

    /// Session duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_duration_seconds: Option<i64>,

    /// Tools used in the session
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_used: Vec<String>,

    /// Models used in the session
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models_used: Vec<String>,

    /// Primary model ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_model_id: Option<String>,

    // === Latency Metrics ===
    /// Average time to first token in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_time_to_first_token_ms: Option<i64>,

    /// Average total response time in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_response_time_ms: Option<i64>,

    /// Minimum time to first token in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_time_to_first_token_ms: Option<i64>,

    /// Maximum time to first token in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_time_to_first_token_ms: Option<i64>,

    /// Number of latency samples
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_sample_count: Option<i64>,

    // === Inter-Token Latency (ITL) Metrics ===
    /// Most recent response's ITL p50 in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_itl_p50_ms: Option<i64>,
    /// Most recent response's ITL p99 in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_itl_p99_ms: Option<i64>,
    /// Worst-case (max across all responses) ITL max in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worst_itl_max_ms: Option<i64>,
    /// Weighted running average of per-response ITL mean in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_itl_mean_ms: Option<i64>,
    /// Total content chunks received across all responses
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_chunk_count: Option<i64>,
    /// Number of responses measured for ITL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_sample_count: Option<i64>,

    // === LOC Attribution ===
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_lines_added: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_lines_removed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_lines_added_reverted: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_lines_removed_reverted: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_lines_added: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_lines_removed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_lines_added_reverted: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_lines_removed_reverted: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_files_touched: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_files_touched: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_files_touched: Option<i64>,

    // === Inference Idle Timeout Tracing ===
    /// Number of inference idle timeout events in this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_idle_timeouts: Option<i64>,
    /// Configured idle timeout threshold (seconds) — set once at session start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_idle_timeout_configured_secs: Option<i64>,

    // === Doom Loop Detection Tracing ===
    /// Number of doom loop warnings (model warned but continued).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_warnings: Option<i64>,
    /// Number of doom loop terminations (turn force-stopped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_terminations: Option<i64>,
    /// Configured repeat threshold — set once at session start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_threshold: Option<i64>,
    /// Configured read-only repeat threshold — set once at session start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_ro_threshold: Option<i64>,
    /// Whether server-side doom-loop recovery resampled at least once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_recovery_fired: Option<bool>,
    /// Number of doom-loop recovery resamples in this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_recovery_attempts: Option<i64>,
    /// Completed responses accepted with confident doom-loop signals after
    /// the resample budget was spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_recovery_accepted_after_budget: Option<i64>,
    /// Tightest (lowest-threshold) raw trigger label observed, e.g.
    /// `tail_repetition:4@thinking`. Labels only — never content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_recovery_top_trigger: Option<String>,
    /// Stream chunks consumed by doomed attempts at their abort points,
    /// summed across resamples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_recovery_aborted_chunks: Option<i64>,

    // === GCS Upload Queue Tracing ===
    /// Total items enqueued for background upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_enqueued: Option<i64>,
    /// Successful background uploads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_uploaded: Option<i64>,
    /// Items that exhausted retry budget (superset of expired).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_failed: Option<i64>,
    /// Enqueue failures that fell back to inline upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_fallbacks: Option<i64>,
    /// Circuit breaker activations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_circuit_breaker_trips: Option<i64>,
    /// Current queue depth (snapshot gauge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_pending: Option<i64>,
    /// Current disk usage of queue temp dir in bytes (snapshot gauge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_pending_bytes: Option<i64>,
    /// Orphaned temp files cleaned up at startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_queue_orphans_cleaned: Option<i64>,

    /// Additional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Session signals data (for GET response).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSignals {
    /// Session ID
    pub session_id: String,

    /// User ID (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// Client type
    pub client_type: ClientType,

    /// When the session started
    pub session_start_at: DateTime<Utc>,

    /// Last activity timestamp
    pub last_activity_at: DateTime<Utc>,

    /// Session duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_duration_seconds: Option<i64>,

    /// Total turns
    pub total_turns: i64,

    /// User message count
    pub user_message_count: i64,

    /// Assistant message count
    pub assistant_message_count: i64,

    /// Cancellation count
    pub cancellation_count: i64,

    /// Consecutive cancellations
    pub consecutive_cancellations: i64,

    /// Error count
    pub error_count: i64,

    /// Tool failure count
    pub tool_failure_count: i64,

    /// Compaction count
    pub compaction_count: i64,

    /// Regeneration count
    pub regeneration_count: i64,

    /// Tools used
    pub tools_used: Vec<String>,

    /// Models used
    pub models_used: Vec<String>,

    /// Primary model ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_model_id: Option<String>,

    // === Latency Metrics ===
    /// Average time to first token in milliseconds
    #[serde(default)]
    pub avg_time_to_first_token_ms: i64,

    /// Average total response time in milliseconds
    #[serde(default)]
    pub avg_response_time_ms: i64,

    /// Minimum time to first token in milliseconds
    #[serde(default)]
    pub min_time_to_first_token_ms: i64,

    /// Maximum time to first token in milliseconds
    #[serde(default)]
    pub max_time_to_first_token_ms: i64,

    /// Number of latency samples
    #[serde(default)]
    pub latency_sample_count: i64,

    // === Inter-Token Latency (ITL) Metrics ===
    /// Most recent response's ITL p50 in milliseconds (nullable: None = not yet reported)
    #[serde(default)]
    pub last_itl_p50_ms: Option<i64>,
    /// Most recent response's ITL p99 in milliseconds (nullable: None = not yet reported)
    #[serde(default)]
    pub last_itl_p99_ms: Option<i64>,
    /// Worst-case (max across all responses) ITL max in milliseconds
    #[serde(default)]
    pub worst_itl_max_ms: i64,
    /// Weighted running average of per-response ITL mean in milliseconds
    #[serde(default)]
    pub avg_itl_mean_ms: i64,
    /// Total content chunks received across all responses
    #[serde(default)]
    pub total_chunk_count: i64,
    /// Number of responses measured for ITL
    #[serde(default)]
    pub itl_sample_count: i64,

    /// Feedback requests sent
    pub feedback_requests_sent: i64,

    /// Feedback requests completed
    pub feedback_requests_completed: i64,

    /// Last feedback request timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_feedback_request_at: Option<DateTime<Utc>>,

    /// Record created at
    pub created_at: DateTime<Utc>,

    /// Record updated at
    pub updated_at: DateTime<Utc>,

    /// Additional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Response for session signals update.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSignalsUpdateResponse {
    /// Session ID
    pub session_id: String,
    /// Timestamp when signals were updated
    pub updated_at: DateTime<Utc>,
}

/// Pending feedback request (returned by GET /v1/feedback/requests).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackRequest {
    /// Unique request ID
    pub request_id: String,

    /// Session ID
    pub session_id: String,

    /// What kind of feedback to collect
    pub feedback_mode: FeedbackMode,

    /// Custom prompt to show user
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_prompt: Option<String>,

    /// Priority (1-10, higher = more important)
    pub priority: i32,

    /// Which heuristic triggered this request
    pub trigger_type: String,

    /// Human-readable reason for the request
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_reason: Option<String>,

    /// Context type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_type: Option<ContextType>,

    /// Specific message IDs to rate
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_message_ids: Vec<String>,

    /// Current status
    pub status: FeedbackRequestStatus,

    /// When the request was created
    pub created_at: DateTime<Utc>,

    /// When the request expires
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    /// Experiment ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,

    /// Comparison ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_id: Option<String>,
}

/// Query parameters for GET /v1/feedback/requests.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct FeedbackRequestsQuery {
    /// Session ID to filter by
    pub session_id: String,
    /// Status filter (defaults to "pending")
    #[serde(default = "default_pending_status")]
    pub status: FeedbackRequestStatus,
}

#[allow(dead_code)]
fn default_pending_status() -> FeedbackRequestStatus {
    FeedbackRequestStatus::Pending
}

// ============================================================================
// Feedback Heuristics Configuration
// ============================================================================

/// Configuration for a single feedback tier.
///
/// Each tier has specific thresholds and conditions that must be met
/// for feedback to be requested at that tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TierConfig {
    /// Whether this tier is enabled
    pub enabled: bool,
    /// Sample rate (0.0 to 1.0, e.g., 0.0005 = 0.05%)
    pub sample_rate: f64,
    /// Minimum turns required to trigger
    pub min_turns: i64,
    /// Minimum tool calls required (Tier 1 & 2)
    #[serde(default)]
    pub min_tool_calls: i64,
    /// Minimum compactions required (Tier 1 & 2)
    #[serde(default)]
    pub min_compactions: i64,
    /// Minimum errors required (Tier 2 only)
    #[serde(default)]
    pub min_errors: i64,
    /// Whether cancellations disqualify this tier (Tier 1)
    #[serde(default)]
    pub no_cancellations: bool,
    /// Whether cancellation is required (Tier 3)
    #[serde(default)]
    pub requires_cancellation: bool,
    /// Whether revert is required (Tier 3)
    #[serde(default)]
    pub requires_revert: bool,
    /// Whether at least one of cancellation/revert is required (Tier 3)
    #[serde(default)]
    pub requires_recovery: bool,
    /// Feedback mode to use when this tier triggers
    pub feedback_mode: FeedbackMode,
    /// Whether feedback requests from this tier are dismissible (non-intrusive)
    #[serde(default = "default_true")]
    pub dismissible: bool,
    /// Prompt text shown to users when this tier's feedback is requested
    #[serde(default)]
    pub prompt: String,
    /// Max times this tier can trigger per session (0 = unlimited)
    #[serde(default = "default_one")]
    pub max_triggers: i32,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_rate: 0.0005,
            min_turns: 10,
            min_tool_calls: 5,
            min_compactions: 2,
            min_errors: 0,
            no_cancellations: false,
            requires_cancellation: false,
            requires_revert: false,
            requires_recovery: false,
            feedback_mode: FeedbackMode::Thumbs,
            dismissible: true,
            prompt: String::new(),
            max_triggers: 1,
        }
    }
}

/// Configuration for feedback heuristics loaded from remote config.
///
/// This struct maps directly to the server's feedback-heuristics config storage.
/// It controls when and how feedback is requested from users.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeedbackHeuristicsConfig {
    /// Unique configuration identifier
    pub config_id: String,
    /// Configuration version (monotonically increasing)
    pub config_version: i64,

    // === Global Settings ===
    /// Master enable/disable switch for all feedback collection
    pub enabled: bool,
    /// Minimum seconds between feedback requests (cooldown period)
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: i64,
    /// Maximum feedback requests per session
    #[serde(default = "default_max_requests")]
    pub max_requests_per_session: i64,

    // === Tier 1: Standard Engagement ===
    /// Whether Tier 1 is enabled
    #[serde(default = "default_true")]
    pub tier1_enabled: bool,
    /// Sample rate for Tier 1 (0.0-1.0)
    #[serde(default = "default_tier1_sample_rate")]
    pub tier1_sample_rate: f64,
    /// Minimum turns for Tier 1
    #[serde(default = "default_tier1_min_turns")]
    pub tier1_min_turns: i64,
    /// Minimum tool calls for Tier 1
    #[serde(default = "default_tier1_min_tool_calls")]
    pub tier1_min_tool_calls: i64,
    /// Minimum compactions for Tier 1
    #[serde(default = "default_tier1_min_compactions")]
    pub tier1_min_compactions: i64,
    /// Whether Tier 1 requires no cancellations
    #[serde(default = "default_true")]
    pub tier1_no_cancellations: bool,
    /// Feedback mode for Tier 1
    #[serde(default = "default_feedback_mode_thumbs")]
    pub tier1_feedback_mode: String,
    /// Whether Tier 1 feedback requests are dismissible
    #[serde(default = "default_true")]
    pub tier1_dismissible: bool,
    /// Prompt text shown to users when Tier 1 feedback is requested
    #[serde(default = "default_tier1_prompt")]
    pub tier1_prompt: String,
    /// Max times Tier 1 can trigger per session (0 = unlimited)
    #[serde(default = "default_one")]
    pub tier1_max_triggers: i32,

    // === Tier 2: Complex Session with Recovery ===
    /// Whether Tier 2 is enabled
    #[serde(default = "default_true")]
    pub tier2_enabled: bool,
    /// Sample rate for Tier 2 (0.0-1.0)
    #[serde(default = "default_tier2_sample_rate")]
    pub tier2_sample_rate: f64,
    /// Minimum turns for Tier 2
    #[serde(default = "default_tier2_min_turns")]
    pub tier2_min_turns: i64,
    /// Minimum tool calls for Tier 2
    #[serde(default = "default_tier2_min_tool_calls")]
    pub tier2_min_tool_calls: i64,
    /// Minimum compactions for Tier 2
    #[serde(default = "default_tier2_min_compactions")]
    pub tier2_min_compactions: i64,
    /// Minimum errors for Tier 2
    #[serde(default = "default_tier2_min_errors")]
    pub tier2_min_errors: i64,
    /// Feedback mode for Tier 2
    #[serde(default = "default_feedback_mode_thumbs_text")]
    pub tier2_feedback_mode: String,
    /// Whether Tier 2 feedback requests are dismissible
    #[serde(default = "default_true")]
    pub tier2_dismissible: bool,
    /// Prompt text shown to users when Tier 2 feedback is requested
    #[serde(default = "default_tier2_prompt")]
    pub tier2_prompt: String,
    /// Max times Tier 2 can trigger per session (0 = unlimited)
    #[serde(default = "default_one")]
    pub tier2_max_triggers: i32,

    // === Tier 3: Recovery from Friction ===
    /// Whether Tier 3 is enabled
    #[serde(default = "default_true")]
    pub tier3_enabled: bool,
    /// Sample rate for Tier 3 (0.0-1.0)
    #[serde(default = "default_tier3_sample_rate")]
    pub tier3_sample_rate: f64,
    /// Minimum turns for Tier 3
    #[serde(default = "default_tier3_min_turns")]
    pub tier3_min_turns: i64,
    /// Whether Tier 3 requires at least one cancellation
    #[serde(default)]
    pub tier3_requires_cancellation: bool,
    /// Whether Tier 3 requires at least one revert
    #[serde(default)]
    pub tier3_requires_revert: bool,
    /// Whether Tier 3 requires recovery (cancellation OR revert)
    #[serde(default = "default_true")]
    pub tier3_requires_recovery: bool,
    /// Feedback mode for Tier 3
    #[serde(default = "default_feedback_mode_stars_text")]
    pub tier3_feedback_mode: String,
    /// Whether Tier 3 feedback requests are dismissible
    #[serde(default = "default_true")]
    pub tier3_dismissible: bool,
    /// Prompt text shown to users when Tier 3 feedback is requested
    #[serde(default = "default_tier3_prompt")]
    pub tier3_prompt: String,
    /// Max times Tier 3 can trigger per session (0 = unlimited)
    #[serde(default = "default_one")]
    pub tier3_max_triggers: i32,

    // === Metadata ===
    /// When this config was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// When this config was last updated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// Who created this config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    // === Lifecycle ===
    /// When this config becomes effective
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    /// When this config expires
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<DateTime<Utc>>,
    /// Whether this config is active
    #[serde(default = "default_true")]
    pub is_active: bool,
    /// User cohorts this config targets (e.g., ["all"], ["beta"])
    #[serde(default = "default_all_cohorts")]
    pub target_user_cohorts: Vec<String>,
    /// Higher priority configs are preferred when multiple match
    #[serde(default)]
    pub priority: i32,
}

impl Default for FeedbackHeuristicsConfig {
    fn default() -> Self {
        Self {
            config_id: "default".to_string(),
            config_version: 1,
            enabled: true,
            cooldown_seconds: 300,
            max_requests_per_session: 3,
            // Tier 1
            tier1_enabled: true,
            tier1_sample_rate: 0.0005,
            tier1_min_turns: 10,
            tier1_min_tool_calls: 5,
            tier1_min_compactions: 2,
            tier1_no_cancellations: true,
            tier1_feedback_mode: "thumbs".to_string(),
            tier1_dismissible: true,
            tier1_prompt: default_tier1_prompt(),
            tier1_max_triggers: 1,
            // Tier 2
            tier2_enabled: true,
            tier2_sample_rate: 0.0002,
            tier2_min_turns: 15,
            tier2_min_tool_calls: 10,
            tier2_min_compactions: 3,
            tier2_min_errors: 1,
            tier2_feedback_mode: "thumbs_text".to_string(),
            tier2_dismissible: true,
            tier2_prompt: default_tier2_prompt(),
            tier2_max_triggers: 1,
            // Tier 3
            tier3_enabled: true,
            tier3_sample_rate: 0.0001,
            tier3_min_turns: 20,
            tier3_requires_cancellation: false,
            tier3_requires_revert: false,
            tier3_requires_recovery: true,
            tier3_feedback_mode: "stars_text".to_string(),
            tier3_dismissible: true,
            tier3_prompt: default_tier3_prompt(),
            tier3_max_triggers: 1,
            // Metadata
            created_at: None,
            updated_at: None,
            created_by: None,
            description: None,
            // Lifecycle
            effective_from: None,
            effective_until: None,
            is_active: true,
            target_user_cohorts: default_all_cohorts(),
            priority: 0,
        }
    }
}

impl FeedbackHeuristicsConfig {
    /// Get the Tier 1 configuration as a TierConfig.
    pub fn tier1_config(&self) -> TierConfig {
        TierConfig {
            enabled: self.tier1_enabled,
            sample_rate: self.tier1_sample_rate,
            min_turns: self.tier1_min_turns,
            min_tool_calls: self.tier1_min_tool_calls,
            min_compactions: self.tier1_min_compactions,
            min_errors: 0,
            no_cancellations: self.tier1_no_cancellations,
            requires_cancellation: false,
            requires_revert: false,
            requires_recovery: false,
            feedback_mode: parse_feedback_mode_str(&self.tier1_feedback_mode),
            dismissible: self.tier1_dismissible,
            prompt: self.tier1_prompt.clone(),
            max_triggers: self.tier1_max_triggers,
        }
    }

    /// Get the Tier 2 configuration as a TierConfig.
    pub fn tier2_config(&self) -> TierConfig {
        TierConfig {
            enabled: self.tier2_enabled,
            sample_rate: self.tier2_sample_rate,
            min_turns: self.tier2_min_turns,
            min_tool_calls: self.tier2_min_tool_calls,
            min_compactions: self.tier2_min_compactions,
            min_errors: self.tier2_min_errors,
            no_cancellations: false,
            requires_cancellation: false,
            requires_revert: false,
            requires_recovery: false,
            feedback_mode: parse_feedback_mode_str(&self.tier2_feedback_mode),
            dismissible: self.tier2_dismissible,
            prompt: self.tier2_prompt.clone(),
            max_triggers: self.tier2_max_triggers,
        }
    }

    /// Get the Tier 3 configuration as a TierConfig.
    pub fn tier3_config(&self) -> TierConfig {
        TierConfig {
            enabled: self.tier3_enabled,
            sample_rate: self.tier3_sample_rate,
            min_turns: self.tier3_min_turns,
            min_tool_calls: 0,
            min_compactions: 0,
            min_errors: 0,
            no_cancellations: false,
            requires_cancellation: self.tier3_requires_cancellation,
            requires_revert: self.tier3_requires_revert,
            requires_recovery: self.tier3_requires_recovery,
            feedback_mode: parse_feedback_mode_str(&self.tier3_feedback_mode),
            dismissible: self.tier3_dismissible,
            prompt: self.tier3_prompt.clone(),
            max_triggers: self.tier3_max_triggers,
        }
    }
}

// Default value functions for serde
fn default_true() -> bool {
    true
}
fn default_cooldown_seconds() -> i64 {
    300
}
fn default_max_requests() -> i64 {
    3
}
fn default_tier1_sample_rate() -> f64 {
    0.0005
}
fn default_tier1_min_turns() -> i64 {
    10
}
fn default_tier1_min_tool_calls() -> i64 {
    5
}
fn default_tier1_min_compactions() -> i64 {
    2
}
fn default_tier2_sample_rate() -> f64 {
    0.0002
}
fn default_tier2_min_turns() -> i64 {
    15
}
fn default_tier2_min_tool_calls() -> i64 {
    10
}
fn default_tier2_min_compactions() -> i64 {
    3
}
fn default_tier2_min_errors() -> i64 {
    1
}
fn default_tier3_sample_rate() -> f64 {
    0.0001
}
fn default_tier3_min_turns() -> i64 {
    20
}
fn default_feedback_mode_thumbs() -> String {
    "thumbs".to_string()
}
fn default_feedback_mode_thumbs_text() -> String {
    "thumbs_text".to_string()
}
fn default_feedback_mode_stars_text() -> String {
    "stars_text".to_string()
}
fn default_tier1_prompt() -> String {
    "You've been using EchoAgent Code productively! Would you mind sharing quick feedback?"
        .to_string()
}
fn default_tier2_prompt() -> String {
    "You've worked through a complex session. Your feedback would help us improve.".to_string()
}
fn default_tier3_prompt() -> String {
    "Thanks for sticking with us through that session. Got a moment to share feedback?".to_string()
}
fn default_all_cohorts() -> Vec<String> {
    vec!["all".to_string()]
}
fn default_one() -> i32 {
    1
}

/// Parse a feedback mode string to FeedbackMode enum.
pub fn parse_feedback_mode_str(s: &str) -> FeedbackMode {
    match s {
        "thumbs" => FeedbackMode::Thumbs,
        "stars" => FeedbackMode::Stars,
        "text" => FeedbackMode::Text,
        "thumbs_text" => FeedbackMode::ThumbsText,
        "stars_text" => FeedbackMode::StarsText,
        "comparison" => FeedbackMode::Comparison,
        "survey" => FeedbackMode::Survey,
        "nps" => FeedbackMode::Nps,
        "nps_text" => FeedbackMode::NpsText,
        _ => FeedbackMode::Thumbs,
    }
}

// ============================================================================
// Session Turn Deltas — per-turn time-series data for regression tracking
// ============================================================================

/// Per-turn delta sent at the end of every turn.
///
/// Contains the *change* in session counters since the previous turn end,
/// plus absolute values for contextual fields (latency, model, experiment info).
/// This produces a time-series row per turn that can be used for regression
/// detection and session-level analytics.
///
/// POST /v1/sessions/{session_id}/turn-deltas
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurnDelta {
    pub client_type: ClientType,
    pub turn_number: i64,
    // Delta counters
    pub delta_tool_calls: i64,
    pub delta_tool_failures: i64,
    pub delta_errors: i64,
    pub delta_cancellations: i64,
    pub delta_regenerations: i64,
    pub delta_compactions: i64,
    pub delta_edit_and_retries: i64,
    pub delta_positive_ratings: i64,
    pub delta_negative_ratings: i64,
    pub delta_assistant_messages: i64,
    pub delta_long_pauses: i64,
    pub delta_successful_tool_uses: i64,
    // Turn-level snapshot values
    pub consecutive_cancellations: i64,
    // Turn-level absolute values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_response_time_ms: Option<i64>,
    /// Inter-token latency p50 for this turn's response (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_p50_ms: Option<i64>,
    /// Inter-token latency p99 for this turn's response (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_p99_ms: Option<i64>,
    /// Inter-token latency max for this turn's response (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_max_ms: Option<i64>,
    /// Inter-token latency mean for this turn's response (ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_mean_ms: Option<i64>,
    pub context_window_usage: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Whole-turn wall-clock duration (prompt→final response), ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_duration_ms: Option<i64>,
    /// Terminal outcome: "completed" | "cancelled" | "error".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_outcome: Option<String>,
    /// Served model fingerprint (provider `system_fingerprint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_used_this_turn: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_types_this_turn: Vec<String>,
    /// Per-tool success/failure breakdown for this turn, JSON-serialized.
    /// Each entry: `{"toolName":"bash","successes":2,"failures":1}`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_outcomes: String,
    // Cumulative totals
    pub cumulative_tool_calls: i64,
    pub cumulative_errors: i64,
    pub session_duration_seconds: i64,
    /// Cumulative total tokens across all compactions
    #[serde(default)]
    pub total_tokens_before_compaction: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Prompt/request ID for the turn (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Session start time — used for analytics ingestion. If not provided, defaults
    /// to the server receipt time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_start_at: Option<DateTime<Utc>>,
    /// Number of feedback requests sent this session (cumulative).
    #[serde(default)]
    pub feedback_requests_sent: i64,
    /// Wall-clock timestamp of the last feedback request sent this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_feedback_request_at: Option<DateTime<Utc>>,
    /// Number of response (completion - reasoning) tokens generated this turn.
    /// `None` when not reported (old client or no inference this turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_tokens: Option<i64>,
    /// Number of thinking (reasoning) tokens generated this turn.
    /// `None` when not reported (old client or no inference this turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<i64>,

    // === LOC Attribution Deltas ===
    #[serde(default)]
    pub delta_agent_lines_added: i64,
    #[serde(default)]
    pub delta_agent_lines_removed: i64,
    #[serde(default)]
    pub delta_agent_lines_added_reverted: i64,
    #[serde(default)]
    pub delta_agent_lines_removed_reverted: i64,
    #[serde(default)]
    pub delta_human_lines_added: i64,
    #[serde(default)]
    pub delta_human_lines_removed: i64,
    #[serde(default)]
    pub delta_human_lines_added_reverted: i64,
    #[serde(default)]
    pub delta_human_lines_removed_reverted: i64,
    #[serde(default)]
    pub delta_agent_files_touched: i64,
    #[serde(default)]
    pub delta_human_files_touched: i64,
    #[serde(default)]
    pub delta_total_files_touched: i64,
    /// Whether LOC tracking was enabled on the client for this session.
    /// When `false`, all `delta_*` LOC fields are meaningless zeros (the
    /// hunk tracker was never spawned). When `true`, zeros indicate the
    /// tracker was active but no code changed during this turn.
    /// Defaults to `false` for backwards-compat with old clients.
    #[serde(default)]
    pub loc_tracking_enabled: bool,
}

/// Response for POST /v1/sessions/{session_id}/turn-deltas.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurnDeltaResponse {
    pub session_id: String,
    pub turn_number: i64,
    pub recorded_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify backward compatibility: JSON from old agents (no ITL fields)
    /// deserializes cleanly, and new agents' ITL fields are also parsed.
    #[test]
    fn session_signals_update_itl_backward_compat() {
        // Old agent: no ITL fields at all
        let json_no_itl = r#"{"clientType": "agent"}"#;
        let update: SessionSignalsUpdate = serde_json::from_str(json_no_itl).unwrap();
        assert_eq!(update.last_itl_p50_ms, None);
        assert_eq!(update.last_itl_p99_ms, None);
        assert_eq!(update.worst_itl_max_ms, None);
        assert_eq!(update.avg_itl_mean_ms, None);
        assert_eq!(update.total_chunk_count, None);
        assert_eq!(update.itl_sample_count, None);

        // New agent: ITL fields present
        let json_with_itl = r#"{"clientType": "agent", "lastItlP50Ms": 45, "worstItlMaxMs": 500}"#;
        let update: SessionSignalsUpdate = serde_json::from_str(json_with_itl).unwrap();
        assert_eq!(update.last_itl_p50_ms, Some(45));
        assert_eq!(update.worst_itl_max_ms, Some(500));
        // Fields not provided should remain None
        assert_eq!(update.last_itl_p99_ms, None);

        // Explicit null
        let json_null_itl = r#"{"clientType": "agent", "lastItlP50Ms": null}"#;
        let update: SessionSignalsUpdate = serde_json::from_str(json_null_itl).unwrap();
        assert_eq!(update.last_itl_p50_ms, None);
    }

    /// Verify backward compatibility for the per-turn latency fields on
    /// `SessionTurnDelta`: JSON from old agents (no turn-duration / outcome /
    /// fingerprint) deserializes with those fields as `None`, and new agents'
    /// values parse correctly. Required (non-`Option`) counters must be present
    /// because they have no serde default.
    #[test]
    fn session_turn_delta_latency_backward_compat() {
        // Old agent: required counters present, no new turn-latency fields.
        let json_old = r#"{
            "clientType": "agent",
            "turnNumber": 1,
            "deltaToolCalls": 0,
            "deltaToolFailures": 0,
            "deltaErrors": 0,
            "deltaCancellations": 0,
            "deltaRegenerations": 0,
            "deltaCompactions": 0,
            "deltaEditAndRetries": 0,
            "deltaPositiveRatings": 0,
            "deltaNegativeRatings": 0,
            "deltaAssistantMessages": 0,
            "deltaLongPauses": 0,
            "deltaSuccessfulToolUses": 0,
            "consecutiveCancellations": 0,
            "contextWindowUsage": 0,
            "cumulativeToolCalls": 0,
            "cumulativeErrors": 0,
            "sessionDurationSeconds": 0
        }"#;
        let delta_old: SessionTurnDelta = serde_json::from_str(json_old).unwrap();
        assert_eq!(delta_old.turn_duration_ms, None);
        assert_eq!(delta_old.turn_outcome, None);
        assert_eq!(delta_old.model_fingerprint, None);

        // Re-serializing an old delta omits the new fields.
        let reserialized = serde_json::to_string(&delta_old).unwrap();
        assert!(!reserialized.contains("turnDurationMs"));
        assert!(!reserialized.contains("turnOutcome"));
        assert!(!reserialized.contains("modelFingerprint"));

        // New agent: turn-latency fields present and parsed.
        let json_new = r#"{
            "clientType": "agent",
            "turnNumber": 2,
            "deltaToolCalls": 0,
            "deltaToolFailures": 0,
            "deltaErrors": 0,
            "deltaCancellations": 0,
            "deltaRegenerations": 0,
            "deltaCompactions": 0,
            "deltaEditAndRetries": 0,
            "deltaPositiveRatings": 0,
            "deltaNegativeRatings": 0,
            "deltaAssistantMessages": 0,
            "deltaLongPauses": 0,
            "deltaSuccessfulToolUses": 0,
            "consecutiveCancellations": 0,
            "contextWindowUsage": 0,
            "cumulativeToolCalls": 0,
            "cumulativeErrors": 0,
            "sessionDurationSeconds": 0,
            "turnDurationMs": 4200,
            "turnOutcome": "completed",
            "modelFingerprint": "fp_abc123"
        }"#;
        let delta_new: SessionTurnDelta = serde_json::from_str(json_new).unwrap();
        assert_eq!(delta_new.turn_duration_ms, Some(4200));
        assert_eq!(delta_new.turn_outcome.as_deref(), Some("completed"));
        assert_eq!(delta_new.model_fingerprint.as_deref(), Some("fp_abc123"));
    }

    /// Verify backward compatibility: JSON without tracing fields (old clients)
    /// deserializes cleanly, and new clients' tracing fields are also parsed.
    #[test]
    fn session_signals_update_tracing_backward_compat() {
        // Old client: no tracing fields — all default to None
        let json_old = r#"{"clientType": "agent"}"#;
        let update: SessionSignalsUpdate = serde_json::from_str(json_old).unwrap();
        assert_eq!(update.inference_idle_timeouts, None);
        assert_eq!(update.inference_idle_timeout_configured_secs, None);
        assert_eq!(update.doom_loop_warnings, None);
        assert_eq!(update.doom_loop_terminations, None);
        assert_eq!(update.doom_loop_threshold, None);
        assert_eq!(update.doom_loop_ro_threshold, None);
        assert_eq!(update.doom_loop_recovery_fired, None);
        assert_eq!(update.doom_loop_recovery_attempts, None);
        assert_eq!(update.doom_loop_recovery_accepted_after_budget, None);
        assert_eq!(update.doom_loop_recovery_top_trigger, None);
        assert_eq!(update.doom_loop_recovery_aborted_chunks, None);
        assert_eq!(update.gcs_queue_enqueued, None);
        assert_eq!(update.gcs_queue_uploaded, None);
        assert_eq!(update.gcs_queue_failed, None);
        assert_eq!(update.gcs_queue_fallbacks, None);
        assert_eq!(update.gcs_queue_circuit_breaker_trips, None);
        assert_eq!(update.gcs_queue_pending, None);
        assert_eq!(update.gcs_queue_pending_bytes, None);
        assert_eq!(update.gcs_queue_orphans_cleaned, None);

        // New client: tracing fields present
        let json_new = r#"{
            "clientType": "agent",
            "inferenceIdleTimeouts": 2,
            "inferenceIdleTimeoutConfiguredSecs": 300,
            "doomLoopWarnings": 1,
            "doomLoopTerminations": 0,
            "doomLoopThreshold": 4,
            "doomLoopRoThreshold": 8,
            "doomLoopRecoveryFired": true,
            "doomLoopRecoveryAttempts": 2,
            "doomLoopRecoveryAcceptedAfterBudget": 1,
            "doomLoopRecoveryTopTrigger": "tail_repetition:4@thinking",
            "doomLoopRecoveryAbortedChunks": 421,
            "gcsQueueEnqueued": 50,
            "gcsQueueUploaded": 48,
            "gcsQueueFailed": 1,
            "gcsQueueFallbacks": 1,
            "gcsQueueCircuitBreakerTrips": 0,
            "gcsQueuePending": 3,
            "gcsQueuePendingBytes": 1048576,
            "gcsQueueOrphansCleaned": 2
        }"#;
        let update: SessionSignalsUpdate = serde_json::from_str(json_new).unwrap();
        assert_eq!(update.inference_idle_timeouts, Some(2));
        assert_eq!(update.inference_idle_timeout_configured_secs, Some(300));
        assert_eq!(update.doom_loop_warnings, Some(1));
        assert_eq!(update.doom_loop_terminations, Some(0));
        assert_eq!(update.doom_loop_threshold, Some(4));
        assert_eq!(update.doom_loop_ro_threshold, Some(8));
        assert_eq!(update.doom_loop_recovery_fired, Some(true));
        assert_eq!(update.doom_loop_recovery_attempts, Some(2));
        assert_eq!(update.doom_loop_recovery_accepted_after_budget, Some(1));
        assert_eq!(
            update.doom_loop_recovery_top_trigger.as_deref(),
            Some("tail_repetition:4@thinking")
        );
        assert_eq!(update.doom_loop_recovery_aborted_chunks, Some(421));
        assert_eq!(update.gcs_queue_enqueued, Some(50));
        assert_eq!(update.gcs_queue_uploaded, Some(48));
        assert_eq!(update.gcs_queue_failed, Some(1));
        assert_eq!(update.gcs_queue_fallbacks, Some(1));
        assert_eq!(update.gcs_queue_circuit_breaker_trips, Some(0));
        assert_eq!(update.gcs_queue_pending, Some(3));
        assert_eq!(update.gcs_queue_pending_bytes, Some(1048576));
        assert_eq!(update.gcs_queue_orphans_cleaned, Some(2));

        // Round-trip: serialize → deserialize preserves values
        let serialized = serde_json::to_string(&update).unwrap();
        let round_tripped: SessionSignalsUpdate = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_tripped.inference_idle_timeouts, Some(2));
        assert_eq!(round_tripped.gcs_queue_uploaded, Some(48));
        assert_eq!(round_tripped.doom_loop_warnings, Some(1));
    }

    /// Verify backward compatibility: JSON from old agents (no token fields)
    /// deserializes cleanly as None, and new agents' token fields are parsed.
    #[test]
    fn session_turn_delta_token_backward_compat() {
        // Old agent: no token fields at all — should deserialize as None
        let json_no_tokens = r#"{
            "clientType": "agent",
            "turnNumber": 1,
            "deltaToolCalls": 0,
            "deltaToolFailures": 0,
            "deltaErrors": 0,
            "deltaCancellations": 0,
            "deltaRegenerations": 0,
            "deltaCompactions": 0,
            "deltaEditAndRetries": 0,
            "deltaPositiveRatings": 0,
            "deltaNegativeRatings": 0,
            "deltaAssistantMessages": 1,
            "deltaLongPauses": 0,
            "deltaSuccessfulToolUses": 0,
            "consecutiveCancellations": 0,
            "contextWindowUsage": 50,
            "cumulativeToolCalls": 0,
            "cumulativeErrors": 0,
            "sessionDurationSeconds": 10
        }"#;
        let delta: SessionTurnDelta = serde_json::from_str(json_no_tokens).unwrap();
        assert_eq!(delta.response_tokens, None);
        assert_eq!(delta.thinking_tokens, None);

        // New agent: token fields present
        let json_with_tokens = r#"{
            "clientType": "agent",
            "turnNumber": 1,
            "deltaToolCalls": 0,
            "deltaToolFailures": 0,
            "deltaErrors": 0,
            "deltaCancellations": 0,
            "deltaRegenerations": 0,
            "deltaCompactions": 0,
            "deltaEditAndRetries": 0,
            "deltaPositiveRatings": 0,
            "deltaNegativeRatings": 0,
            "deltaAssistantMessages": 1,
            "deltaLongPauses": 0,
            "deltaSuccessfulToolUses": 0,
            "consecutiveCancellations": 0,
            "contextWindowUsage": 50,
            "cumulativeToolCalls": 0,
            "cumulativeErrors": 0,
            "sessionDurationSeconds": 10,
            "responseTokens": 512,
            "thinkingTokens": 1024
        }"#;
        let delta: SessionTurnDelta = serde_json::from_str(json_with_tokens).unwrap();
        assert_eq!(delta.response_tokens, Some(512));
        assert_eq!(delta.thinking_tokens, Some(1024));
    }

    /// Verify backward compatibility: JSON from old agents (no `locTrackingEnabled`)
    /// defaults to `false`, and new agents that send `true` are parsed correctly.
    #[test]
    fn session_turn_delta_loc_tracking_backward_compat() {
        // Old agent: no locTrackingEnabled field — should default to false
        let json_no_loc = r#"{
            "clientType": "agent",
            "turnNumber": 1,
            "deltaToolCalls": 0,
            "deltaToolFailures": 0,
            "deltaErrors": 0,
            "deltaCancellations": 0,
            "deltaRegenerations": 0,
            "deltaCompactions": 0,
            "deltaEditAndRetries": 0,
            "deltaPositiveRatings": 0,
            "deltaNegativeRatings": 0,
            "deltaAssistantMessages": 1,
            "deltaLongPauses": 0,
            "deltaSuccessfulToolUses": 0,
            "consecutiveCancellations": 0,
            "contextWindowUsage": 50,
            "cumulativeToolCalls": 0,
            "cumulativeErrors": 0,
            "sessionDurationSeconds": 10
        }"#;
        let delta: SessionTurnDelta = serde_json::from_str(json_no_loc).unwrap();
        assert!(
            !delta.loc_tracking_enabled,
            "old clients should default to false"
        );
        // LOC fields should be 0 (serde default)
        assert_eq!(delta.delta_agent_lines_added, 0);

        // New agent with LOC tracking enabled
        let json_with_loc = r#"{
            "clientType": "agent",
            "turnNumber": 2,
            "deltaToolCalls": 0,
            "deltaToolFailures": 0,
            "deltaErrors": 0,
            "deltaCancellations": 0,
            "deltaRegenerations": 0,
            "deltaCompactions": 0,
            "deltaEditAndRetries": 0,
            "deltaPositiveRatings": 0,
            "deltaNegativeRatings": 0,
            "deltaAssistantMessages": 1,
            "deltaLongPauses": 0,
            "deltaSuccessfulToolUses": 0,
            "consecutiveCancellations": 0,
            "contextWindowUsage": 50,
            "cumulativeToolCalls": 0,
            "cumulativeErrors": 0,
            "sessionDurationSeconds": 20,
            "locTrackingEnabled": true,
            "deltaAgentLinesAdded": 10
        }"#;
        let delta: SessionTurnDelta = serde_json::from_str(json_with_loc).unwrap();
        assert!(delta.loc_tracking_enabled);
        assert_eq!(delta.delta_agent_lines_added, 10);
    }

    /// Old agents that predate cohort targeting send no cohort fields.
    /// Verify they deserialize to defaults (target_user_cohorts=["all"], priority=0)
    /// and that new agents' cohort fields are parsed correctly.
    #[test]
    fn feedback_heuristics_config_cohort_backward_compat() {
        // Old config: no cohort fields → defaults to ["all"] and priority 0
        let json_no_cohorts = r#"{"config_id":"v1","config_version":1,"enabled":true}"#;
        let config: FeedbackHeuristicsConfig = serde_json::from_str(json_no_cohorts).unwrap();
        assert_eq!(config.target_user_cohorts, vec!["all"]);
        assert_eq!(config.priority, 0);

        // New config: cohort fields present
        let json_with_cohorts = r#"{
            "config_id":"v2","config_version":2,"enabled":true,
            "target_user_cohorts":["beta"],"priority":10
        }"#;
        let config: FeedbackHeuristicsConfig = serde_json::from_str(json_with_cohorts).unwrap();
        assert_eq!(config.target_user_cohorts, vec!["beta"]);
        assert_eq!(config.priority, 10);

        // Round-trip: serialize → deserialize preserves cohort values
        let serialized = serde_json::to_string(&config).unwrap();
        let round_tripped: FeedbackHeuristicsConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_tripped.target_user_cohorts, vec!["beta"]);
        assert_eq!(round_tripped.priority, 10);
    }

    fn feedback_image(encoded_len: usize, mime_type: &str) -> FeedbackImage {
        FeedbackImage {
            data: "A".repeat(encoded_len),
            mime_type: mime_type.to_string(),
            file_name: None,
        }
    }

    #[test]
    fn feedback_submission_images_backward_compat() {
        let legacy = r#"{"sessionId":"s1","clientType":"tui","feedbackType":"text"}"#;
        let submission: FeedbackSubmission = serde_json::from_str(legacy).unwrap();
        assert!(submission.images.is_empty());
        assert!(
            !serde_json::to_string(&submission)
                .unwrap()
                .contains("images")
        );

        let mut with_image = submission.clone();
        with_image.images = vec![FeedbackImage {
            data: "aGk=".into(),
            mime_type: "image/png".into(),
            file_name: Some("shot.png".into()),
        }];
        let round_tripped: FeedbackSubmission =
            serde_json::from_str(&serde_json::to_string(&with_image).unwrap()).unwrap();
        assert_eq!(round_tripped.images.len(), 1);
        assert_eq!(round_tripped.images[0].data, "aGk=");
        assert_eq!(round_tripped.images[0].mime_type, "image/png");
        assert_eq!(
            round_tripped.images[0].file_name.as_deref(),
            Some("shot.png")
        );
    }

    #[test]
    fn validate_feedback_images_enforces_shared_limits() {
        assert!(validate_feedback_images(&[]).is_ok());
        assert!(validate_feedback_images(&[feedback_image(100, "image/png")]).is_ok());

        let too_many = vec![feedback_image(4, "image/png"); MAX_FEEDBACK_IMAGES + 1];
        assert_eq!(
            validate_feedback_images(&too_many),
            Err(FeedbackImageError::TooManyImages {
                count: MAX_FEEDBACK_IMAGES + 1
            })
        );

        assert_eq!(
            validate_feedback_images(&[feedback_image(100, "image/tiff")]),
            Err(FeedbackImageError::UnsupportedMimeType {
                index: 0,
                mime_type: "image/tiff".into()
            })
        );

        let oversized = feedback_image(MAX_FEEDBACK_IMAGE_BYTES / 3 * 4 + 8, "image/png");
        assert_eq!(
            validate_feedback_images(&[oversized]),
            Err(FeedbackImageError::ImageTooLarge { index: 0 })
        );

        // Individually valid, collectively over the total budget.
        let per_image = MAX_FEEDBACK_IMAGE_TOTAL_BYTES / 3 * 4 / 3;
        let over_total = vec![feedback_image(per_image, "image/png"); 4];
        assert_eq!(
            validate_feedback_images(&over_total),
            Err(FeedbackImageError::TotalTooLarge)
        );
    }

    #[test]
    fn decoded_len_is_exact() {
        let cases = [
            ("", 0),
            ("YQ==", 1),
            ("YQ", 1),
            ("YWI=", 2),
            ("YWI", 2),
            ("YWJj", 3),
            ("YWJjZGVmZ2hp", 9),
        ];
        for (encoded, decoded_len) in cases {
            let image = FeedbackImage {
                data: encoded.to_string(),
                mime_type: "image/png".into(),
                file_name: None,
            };
            assert_eq!(image.decoded_len(), decoded_len, "{encoded:?}");
        }
    }
}
