//! Card state and transitions for the MCP elicitation card.
//!
//! The lifecycle is a data-carrying enum: [`ElicitationStage::Form`] and
//! [`ElicitationStage::UrlConsent`] own the pending ACP responder;
//! [`ElicitationStage::UrlWaiting`] exists only after the response was sent,
//! so "still owes a response" is encoded by the stage itself.

use agent_client_protocol as acp;
use echo_agent_acp::AcpResult;
use echo_agent_tools::mcp_elicitation::{
    ElicitFieldKind, ElicitFieldSpec, ElicitFieldValue, MAX_ELICIT_DESC_CHARS,
    MAX_ELICIT_DRAFT_CHARS, MAX_ELICIT_ENUM_VALUE_CHARS, MAX_ELICIT_MESSAGE_CHARS,
    MAX_ELICIT_TITLE_CHARS, MAX_ELICIT_URL_CHARS, McpElicitExtRequest, McpElicitExtResponse,
    McpElicitModeFields, parse_form_schema, take_chars, validate_form,
};

use crate::views::prompt_widget::StashedPrompt;

pub type ElicitResponseTx = tokio::sync::oneshot::Sender<AcpResult<acp::ExtResponse>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationFocus {
    Fields,
    /// Text-field editing, or option-walking inside an expanded
    /// multi-select — both leave back to [`Self::Fields`] on Esc.
    Editing,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationActionFocus {
    Accept,
    Decline,
}

pub struct ElicitationViewState {
    pub tool_call_id: String,
    pub server_name: String,
    /// Verbatim wire `serverName` for equality against later notifications
    /// (the display `server_name` above is sanitized/truncated).
    pub server_name_wire: String,
    pub message: String,
    pub stage: ElicitationStage,
    pub focus: ElicitationFocus,
    pub action_focus: ElicitationActionFocus,
    /// First visible body row; the renderer clamps it and keeps the cursor
    /// row in view.
    pub scroll: usize,
    /// The composer draft this card displaced, or `None` when an earlier card
    /// (permission / question / plan approval) already held the session draft —
    /// then the live composer was not ours to stash, and closing must not
    /// restore over whatever that card put back.
    pub stashed_prompt: Option<StashedPrompt>,
}

pub enum ElicitationStage {
    Form(FormStage),
    UrlConsent(UrlConsentStage),
    UrlWaiting(UrlWaitingStage),
}

pub struct FormStage {
    pub fields: Vec<FormFieldUi>,
    pub parse_error: Option<String>,
    pub field_cursor: usize,
    pub response_tx: Option<ElicitResponseTx>,
}

pub struct UrlConsentStage {
    pub display: UrlDisplay,
    /// Why the URL cannot be opened (bad syntax, non-http(s) scheme,
    /// embedded credentials). `Some` disables Accept — the server can then
    /// only receive Decline or Cancel, never a false `accept`.
    pub invalid: Option<String>,
    pub elicitation_id: String,
    pub response_tx: Option<ElicitResponseTx>,
}

pub struct UrlWaitingStage {
    pub display: UrlDisplay,
    pub elicitation_id: String,
}

/// The URL as shown (and opened): normalized by the `url` crate when it
/// parses, with the host split out for emphasis in the card.
pub struct UrlDisplay {
    pub url: String,
    pub host: Option<String>,
    /// Any host label is Punycode (`xn--`) — surfaced as a spoofing warning.
    pub punycode_host: bool,
}

/// The editable value of one form field. Exactly one shape is valid per
/// [`ElicitFieldKind`], so the variant carries only that shape — a boolean
/// cannot hold a draft, a text field cannot hold selections.
pub enum FieldValueUi {
    /// String / Number / Integer: the raw text draft.
    Text {
        draft: String,
    },
    Toggle {
        on: bool,
    },
    /// Single-select: chosen option index.
    Choice {
        index: Option<usize>,
    },
    /// Multi-select: per-option toggles (aligned with the spec's options)
    /// and the highlighted option while the field is expanded.
    Multi {
        selected: Vec<bool>,
        cursor: usize,
    },
    /// Unsupported field kinds have nothing to edit.
    Unavailable,
}

impl FieldValueUi {
    fn from_spec(kind: &ElicitFieldKind) -> Self {
        match kind {
            ElicitFieldKind::String { default, .. }
            | ElicitFieldKind::Number { default, .. }
            | ElicitFieldKind::Integer { default, .. } => Self::Text {
                draft: default.clone().unwrap_or_default(),
            },
            ElicitFieldKind::Boolean { default } => Self::Toggle { on: *default },
            ElicitFieldKind::SingleSelect { default_index, .. } => Self::Choice {
                index: *default_index,
            },
            ElicitFieldKind::MultiSelect {
                options,
                default_indexes,
                ..
            } => {
                let mut selected = vec![false; options.len()];
                for &i in default_indexes {
                    if let Some(slot) = selected.get_mut(i) {
                        *slot = true;
                    }
                }
                Self::Multi {
                    selected,
                    cursor: 0,
                }
            }
            ElicitFieldKind::Unsupported { .. } => Self::Unavailable,
        }
    }

    /// The submitted-value view of this field, borrowing `index_buf` for a
    /// multi-select's selected indexes.
    fn as_submission<'a>(&'a self, index_buf: &'a [usize]) -> ElicitFieldValue<'a> {
        match self {
            Self::Text { draft } => ElicitFieldValue::Draft(draft),
            Self::Toggle { on } => ElicitFieldValue::Bool(*on),
            Self::Choice { index } => ElicitFieldValue::Choice(*index),
            Self::Multi { .. } => ElicitFieldValue::MultiChoice(index_buf),
            Self::Unavailable => ElicitFieldValue::Draft(""),
        }
    }
}

/// Per-field UI state over the immutable schema [`ElicitFieldSpec`]:
/// the editable value and the last validation error shown.
pub struct FormFieldUi {
    pub spec: ElicitFieldSpec,
    pub value: FieldValueUi,
    pub error: Option<String>,
}

impl FormFieldUi {
    fn new(spec: ElicitFieldSpec) -> Self {
        let value = FieldValueUi::from_spec(&spec.kind);
        Self {
            spec,
            value,
            error: None,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self.value, FieldValueUi::Text { .. })
    }

    pub fn is_multi_select(&self) -> bool {
        matches!(self.value, FieldValueUi::Multi { .. })
    }

    pub fn draft(&self) -> &str {
        match &self.value {
            FieldValueUi::Text { draft } => draft,
            _ => "",
        }
    }

    /// Replace a text field's draft (tests and paste-preload paths).
    pub fn set_draft(&mut self, draft: impl Into<String>) {
        if let FieldValueUi::Text { draft: d } = &mut self.value {
            *d = draft.into();
            self.error = None;
        }
    }

    pub fn option_count(&self) -> usize {
        match &self.spec.kind {
            ElicitFieldKind::MultiSelect { options, .. } => options.len(),
            _ => 0,
        }
    }

    pub fn option_cursor(&self) -> usize {
        match &self.value {
            FieldValueUi::Multi { cursor, .. } => *cursor,
            _ => 0,
        }
    }

    pub fn option_selected(&self, index: usize) -> bool {
        match &self.value {
            FieldValueUi::Multi { selected, .. } => selected.get(index).copied().unwrap_or(false),
            _ => false,
        }
    }

    pub fn toggle_option(&mut self, index: usize) {
        if let FieldValueUi::Multi { selected, cursor } = &mut self.value
            && let Some(slot) = selected.get_mut(index)
        {
            *slot = !*slot;
            *cursor = index;
            self.error = None;
        }
    }

    fn selected_indexes(&self) -> Vec<usize> {
        match &self.value {
            FieldValueUi::Multi { selected, .. } => selected
                .iter()
                .enumerate()
                .filter_map(|(i, on)| on.then_some(i))
                .collect(),
            _ => vec![],
        }
    }
}

/// Server-controlled text is painted straight into the terminal buffer:
/// strip the shared unsafe set (escape/control injection, bidi spoofing)
/// before capping length.
fn sanitize_server_text(raw: &str, max_chars: usize) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !crate::render::line_utils::is_unsafe_display_char(*c))
        .collect();
    take_chars(&cleaned, max_chars)
}

/// Sanitize every server-derived display string of a parsed field spec.
/// Option `value`s are echoed back to the server verbatim and never painted
/// (the display side is `label`), so they are left untouched.
fn sanitize_spec(spec: &mut ElicitFieldSpec) {
    spec.title = sanitize_server_text(&spec.title, MAX_ELICIT_TITLE_CHARS);
    if let Some(desc) = &spec.description {
        spec.description = Some(sanitize_server_text(desc, MAX_ELICIT_DESC_CHARS));
    }
    match &mut spec.kind {
        ElicitFieldKind::String { default, .. }
        | ElicitFieldKind::Number { default, .. }
        | ElicitFieldKind::Integer { default, .. } => {
            if let Some(d) = default {
                *default = Some(sanitize_server_text(d, MAX_ELICIT_DRAFT_CHARS));
            }
        }
        ElicitFieldKind::SingleSelect { options, .. }
        | ElicitFieldKind::MultiSelect { options, .. } => {
            for option in options.iter_mut() {
                option.label = sanitize_server_text(&option.label, MAX_ELICIT_ENUM_VALUE_CHARS);
            }
        }
        // `reason` embeds the schema's `type` string verbatim.
        ElicitFieldKind::Unsupported { reason } => {
            *reason = sanitize_server_text(reason, MAX_ELICIT_TITLE_CHARS);
        }
        ElicitFieldKind::Boolean { .. } => {}
    }
}

/// Validate a URL elicitation target before it is ever offered for consent:
/// it must parse, be plain http(s), and carry no embedded credentials. The
/// returned URL is the parser's normalized form (Unicode hosts render as
/// their Punycode labels, which the card then flags).
pub(super) fn check_elicit_url(raw: &str) -> Result<UrlDisplay, String> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| "malformed URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme \"{}\"", parsed.scheme()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL embeds credentials".to_string());
    }
    let Some(host) = parsed.host_str() else {
        return Err("URL has no host".to_string());
    };
    let punycode_host = host.split('.').any(|label| label.starts_with("xn--"));
    Ok(UrlDisplay {
        host: Some(host.to_string()),
        punycode_host,
        url: parsed.to_string(),
    })
}

impl ElicitationViewState {
    pub fn from_request(
        req: McpElicitExtRequest,
        stashed_prompt: Option<StashedPrompt>,
        response_tx: Option<ElicitResponseTx>,
    ) -> Self {
        let (stage, focus) = match req.mode {
            McpElicitModeFields::Form { requested_schema } => {
                let schema = requested_schema.unwrap_or(serde_json::json!({
                    "type": "object",
                    "properties": {}
                }));
                let (fields, parse_error) = match parse_form_schema(&schema) {
                    Ok(mut specs) => {
                        for spec in &mut specs {
                            sanitize_spec(spec);
                        }
                        (specs.into_iter().map(FormFieldUi::new).collect(), None)
                    }
                    Err(e) => (vec![], Some(e)),
                };
                let has_fields = !fields.is_empty();
                let stage = ElicitationStage::Form(FormStage {
                    fields,
                    parse_error,
                    field_cursor: 0,
                    response_tx,
                });
                (
                    stage,
                    if has_fields {
                        ElicitationFocus::Fields
                    } else {
                        ElicitationFocus::Actions
                    },
                )
            }
            McpElicitModeFields::Url {
                url,
                elicitation_id,
            } => {
                let sanitized = sanitize_server_text(&url, MAX_ELICIT_URL_CHARS);
                let (display, invalid) = match check_elicit_url(&sanitized) {
                    Ok(display) => (display, None),
                    Err(reason) => (
                        UrlDisplay {
                            url: sanitized,
                            host: None,
                            punycode_host: false,
                        },
                        Some(reason),
                    ),
                };
                let stage = ElicitationStage::UrlConsent(UrlConsentStage {
                    display,
                    invalid,
                    elicitation_id,
                    response_tx,
                });
                (stage, ElicitationFocus::Actions)
            }
        };

        Self {
            tool_call_id: req.tool_call_id,
            server_name: sanitize_server_text(&req.server_name, MAX_ELICIT_TITLE_CHARS),
            server_name_wire: req.server_name,
            message: sanitize_server_text(&req.message, MAX_ELICIT_MESSAGE_CHARS),
            stage,
            focus,
            action_focus: ElicitationActionFocus::Accept,
            scroll: 0,
            stashed_prompt,
        }
    }

    pub fn title(&self) -> String {
        match &self.stage {
            ElicitationStage::Form(_) => {
                format!("MCP “{}” requests your input", self.server_name)
            }
            ElicitationStage::UrlConsent(_) => {
                format!("MCP “{}” wants to open a URL", self.server_name)
            }
            ElicitationStage::UrlWaiting(_) => {
                format!("MCP “{}”, waiting for completion", self.server_name)
            }
        }
    }

    /// The form-parse or URL-validation error banner, if any.
    pub fn banner_error(&self) -> Option<&str> {
        match &self.stage {
            ElicitationStage::Form(form) => form.parse_error.as_deref(),
            ElicitationStage::UrlConsent(consent) => consent.invalid.as_deref(),
            ElicitationStage::UrlWaiting(_) => None,
        }
    }

    pub fn form(&self) -> Option<&FormStage> {
        match &self.stage {
            ElicitationStage::Form(form) => Some(form),
            _ => None,
        }
    }

    pub fn form_mut(&mut self) -> Option<&mut FormStage> {
        match &mut self.stage {
            ElicitationStage::Form(form) => Some(form),
            _ => None,
        }
    }

    pub fn is_url_waiting(&self) -> bool {
        matches!(self.stage, ElicitationStage::UrlWaiting(_))
    }

    pub fn url(&self) -> Option<&str> {
        match &self.stage {
            ElicitationStage::UrlConsent(consent) => Some(consent.display.url.as_str()),
            ElicitationStage::UrlWaiting(waiting) => Some(waiting.display.url.as_str()),
            ElicitationStage::Form(_) => None,
        }
    }

    pub fn elicitation_id(&self) -> Option<&str> {
        match &self.stage {
            ElicitationStage::UrlConsent(consent) => Some(consent.elicitation_id.as_str()),
            ElicitationStage::UrlWaiting(waiting) => Some(waiting.elicitation_id.as_str()),
            ElicitationStage::Form(_) => None,
        }
    }

    pub fn field_count(&self) -> usize {
        self.form().map(|f| f.fields.len()).unwrap_or(0)
    }

    pub fn current_field(&self) -> Option<&FormFieldUi> {
        let form = self.form()?;
        form.fields.get(form.field_cursor)
    }

    pub fn current_field_mut(&mut self) -> Option<&mut FormFieldUi> {
        let form = self.form_mut()?;
        let cursor = form.field_cursor;
        form.fields.get_mut(cursor)
    }

    pub fn field_cursor(&self) -> usize {
        self.form().map(|f| f.field_cursor).unwrap_or(0)
    }

    pub fn move_field(&mut self, delta: isize) {
        let n = self.field_count();
        if n == 0 {
            self.focus = ElicitationFocus::Actions;
            return;
        }
        let Some(form) = self.form_mut() else {
            return;
        };
        let cur = form.field_cursor as isize + delta;
        if cur < 0 {
            form.field_cursor = 0;
        } else if cur as usize >= n {
            form.field_cursor = n.saturating_sub(1);
            self.focus = ElicitationFocus::Actions;
        } else {
            form.field_cursor = cur as usize;
            self.focus = ElicitationFocus::Fields;
        }
    }

    /// Shared focus movement for the walk keys. `wrap` distinguishes
    /// Tab/Shift+Tab (wrap between the field list and the action rows at both
    /// ends) from Down/Up/j/k (clamp at the edges: Down stops on Decline, Up
    /// stops on the first field). Editing focus never moves from here — the
    /// key handler exits edit mode before dispatching a walk.
    pub fn move_focus(&mut self, forward: bool, wrap: bool) {
        let n = self.field_count();
        match self.focus {
            ElicitationFocus::Fields => {
                let cursor = self.field_cursor();
                if forward {
                    if n == 0 || cursor + 1 >= n {
                        self.focus = ElicitationFocus::Actions;
                        self.action_focus = ElicitationActionFocus::Accept;
                    } else {
                        self.move_field(1);
                    }
                } else if wrap && cursor == 0 {
                    self.focus = ElicitationFocus::Actions;
                    self.action_focus = ElicitationActionFocus::Decline;
                } else {
                    // Clamps at the first field (and parks a zero-field form
                    // on the actions).
                    self.move_field(-1);
                }
            }
            ElicitationFocus::Actions => match (forward, self.action_focus) {
                (true, ElicitationActionFocus::Accept) => {
                    self.action_focus = ElicitationActionFocus::Decline;
                }
                (true, ElicitationActionFocus::Decline) if wrap => {
                    if n > 0 {
                        self.focus = ElicitationFocus::Fields;
                        if let Some(form) = self.form_mut() {
                            form.field_cursor = 0;
                        }
                    } else {
                        self.action_focus = ElicitationActionFocus::Accept;
                    }
                }
                (true, ElicitationActionFocus::Decline) => {}
                (false, ElicitationActionFocus::Decline) => {
                    self.action_focus = ElicitationActionFocus::Accept;
                }
                (false, ElicitationActionFocus::Accept) => {
                    if n > 0 {
                        self.focus = ElicitationFocus::Fields;
                        if let Some(form) = self.form_mut() {
                            form.field_cursor = n - 1;
                        }
                    } else if wrap {
                        self.action_focus = ElicitationActionFocus::Decline;
                    }
                }
            },
            ElicitationFocus::Editing => {}
        }
    }

    /// Toggle a boolean or cycle a single-select on the current field.
    pub fn toggle_bool_or_enum(&mut self) {
        let Some(field) = self.current_field_mut() else {
            return;
        };
        let option_count = match &field.spec.kind {
            ElicitFieldKind::SingleSelect { options, .. } => options.len(),
            _ => 0,
        };
        match &mut field.value {
            FieldValueUi::Toggle { on } => {
                *on = !*on;
                field.error = None;
            }
            FieldValueUi::Choice { index } => {
                if option_count == 0 {
                    return;
                }
                *index = Some(index.map(|i| (i + 1) % option_count).unwrap_or(0));
                field.error = None;
            }
            _ => {}
        }
    }

    /// Enter text editing on a String / Number / Integer field, or expand a
    /// multi-select into option-walking. Both use [`ElicitationFocus::Editing`].
    pub fn enter_edit_or_options(&mut self) -> bool {
        let Some(field) = self.current_field() else {
            return false;
        };
        if field.is_text() || field.is_multi_select() {
            self.focus = ElicitationFocus::Editing;
            true
        } else {
            false
        }
    }

    /// Enter text editing only (typing a character must not expand a
    /// multi-select).
    pub fn enter_edit_if_text(&mut self) -> bool {
        if self.current_field().is_some_and(|f| f.is_text()) {
            self.focus = ElicitationFocus::Editing;
            true
        } else {
            false
        }
    }

    pub fn append_char(&mut self, c: char) {
        // Drafts echo back onto the terminal; reject pasted escapes and
        // bidi/format characters at the single ingestion point.
        if c.is_control() || crate::render::line_utils::is_unsafe_display_char(c) {
            return;
        }
        let Some(field) = self.current_field_mut() else {
            return;
        };
        if let FieldValueUi::Text { draft } = &mut field.value
            && draft.chars().count() < MAX_ELICIT_DRAFT_CHARS
        {
            draft.push(c);
            field.error = None;
        }
    }

    pub fn backspace(&mut self) {
        let Some(field) = self.current_field_mut() else {
            return;
        };
        if let FieldValueUi::Text { draft } = &mut field.value {
            draft.pop();
            field.error = None;
        }
    }

    /// Move the option cursor of the expanded multi-select. Returns false
    /// when the current field is not an expanded multi-select.
    pub fn move_option_cursor(&mut self, delta: isize) -> bool {
        let Some(field) = self.current_field_mut() else {
            return false;
        };
        let n = field.option_count();
        if n == 0 {
            return false;
        }
        if let FieldValueUi::Multi { cursor, .. } = &mut field.value {
            let cur = *cursor as isize + delta;
            *cursor = cur.clamp(0, n as isize - 1) as usize;
            true
        } else {
            false
        }
    }

    pub fn toggle_current_option(&mut self) {
        let Some(field) = self.current_field_mut() else {
            return;
        };
        let cursor = field.option_cursor();
        field.toggle_option(cursor);
    }

    /// Build the Accept response when the card is in an acceptable state:
    /// a form that validates, or a consent-phase URL that passed the URL
    /// checks. `None` leaves the card open (field errors are set on the
    /// form). URL waiting has no response left to build.
    pub fn try_accept(&mut self) -> Option<McpElicitExtResponse> {
        match &mut self.stage {
            ElicitationStage::UrlConsent(consent) => {
                if consent.invalid.is_some() {
                    return None;
                }
                Some(McpElicitExtResponse::Accept { content: None })
            }
            ElicitationStage::UrlWaiting(_) => None,
            ElicitationStage::Form(form) => {
                if form.parse_error.is_some() {
                    return None;
                }
                let specs: Vec<ElicitFieldSpec> =
                    form.fields.iter().map(|f| f.spec.clone()).collect();
                let index_buf: Vec<Vec<usize>> =
                    form.fields.iter().map(|f| f.selected_indexes()).collect();
                let values: Vec<ElicitFieldValue<'_>> = form
                    .fields
                    .iter()
                    .zip(&index_buf)
                    .map(|(f, indexes)| f.value.as_submission(indexes))
                    .collect();
                match validate_form(&specs, &values) {
                    Ok(content) => {
                        for field in &mut form.fields {
                            field.error = None;
                        }
                        Some(McpElicitExtResponse::Accept {
                            content: Some(serde_json::Value::Object(content)),
                        })
                    }
                    Err(errors) => {
                        for field in &mut form.fields {
                            field.error = errors
                                .iter()
                                .find(|e| e.field == field.spec.name)
                                .map(|e| e.message.clone());
                        }
                        None
                    }
                }
            }
        }
    }

    pub fn decline_response() -> McpElicitExtResponse {
        McpElicitExtResponse::Decline
    }

    pub fn cancel_response() -> McpElicitExtResponse {
        McpElicitExtResponse::Cancel
    }

    pub fn take_response_tx(&mut self) -> Option<ElicitResponseTx> {
        match &mut self.stage {
            ElicitationStage::Form(form) => form.response_tx.take(),
            ElicitationStage::UrlConsent(consent) => consent.response_tx.take(),
            ElicitationStage::UrlWaiting(_) => None,
        }
    }

    /// Send the response on the stage's pending responder. Returns whether
    /// it was actually delivered to a live request — `false` means the MCP
    /// side already abandoned it (server cancel / teardown), so callers must
    /// not act as if the server heard the answer (e.g. must not open a URL
    /// or enter the waiting stage).
    #[must_use]
    pub fn send_response(&mut self, response: McpElicitExtResponse) -> bool {
        let Some(tx) = self.take_response_tx() else {
            return false;
        };
        let Ok(raw) = serde_json::value::to_raw_value(&response) else {
            return false;
        };
        tx.send(Ok(acp::ExtResponse::new(raw.into()))).is_ok()
    }

    /// Consent → waiting transition, after the Accept response was sent.
    /// No-op for other stages.
    pub fn begin_url_waiting(&mut self) {
        if let ElicitationStage::UrlConsent(consent) = &mut self.stage {
            debug_assert!(
                consent.response_tx.is_none(),
                "waiting begins only after the consent response was sent"
            );
            let display = UrlDisplay {
                url: std::mem::take(&mut consent.display.url),
                host: consent.display.host.take(),
                punycode_host: consent.display.punycode_host,
            };
            let elicitation_id = std::mem::take(&mut consent.elicitation_id);
            self.stage = ElicitationStage::UrlWaiting(UrlWaitingStage {
                display,
                elicitation_id,
            });
            self.action_focus = ElicitationActionFocus::Accept;
            self.scroll = 0;
        }
    }
}
