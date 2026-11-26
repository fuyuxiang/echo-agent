//! Failed-response context retained for Doom-loop recovery retries.
//!
//! Only model-authored reasoning and visible text are replayed, and only from
//! a turn that made no tool call: Responses reasoning items are bound to the
//! function calls that follow them, so replaying a turn without its calls
//! would send an orphaned reasoning item the API rejects. A turn that called a
//! tool is therefore dropped whole and the retry carries the reminder alone.
//!
//! What the wire delivered as a completed item — the terminal `output` list,
//! or a `response.output_item.done` when the attempt is aborted before its
//! terminal frame — is authoritative for both content and order. Streamed
//! deltas only stand in for an item the wire never completed, so the replay
//! keeps `encrypted_content` and the model's own item ids instead of
//! synthesising plaintext copies. Reading the raw items also keeps the
//! tool-call veto honest: the conversation form the sampler hands upward is
//! lossy (it drops MCP calls) and has already had a streaming-reasoning
//! fallback spliced into it.
//!
//! Capture is allocated only for an attempt whose doom-loop abort is armed
//! ([`FailedResponseCapture::armed`]); every other stream holds the default
//! disarmed handle, whose recording methods are no-ops.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use echo_agent_sampling_types::{ConversationItem, ConversationRequest, rs};

pub(crate) const RECOVERY_REMINDER: &str = "<system_reminder>Your messages have been flagged as looping. Your response has been flagged as repeating the same text pattern. Avoid excessive repetition. If you are having trouble ask the user for guidance.</system_reminder>";

/// Hard caps on the bytes of one failed turn replayed into a retry. A
/// detector can report only at the terminal frame, so the failed turn may be
/// a full generation; every recovery attempt appends another one, and without
/// a cap the retry prompt would grow until it overflowed the context. The
/// reasoning and the visible answer are budgeted separately so a turn that
/// loops in its thinking still replays the answer it did produce.
const MAX_RECOVERY_REASONING_BYTES: usize = 8 * 1024;
const MAX_RECOVERY_TEXT_BYTES: usize = 4 * 1024;

/// Marks the point where the cap cut the failed turn, so the model (and
/// anyone reading the request) can tell replay from a complete turn.
const TRUNCATION_MARKER: &str = " […truncated]";

/// Byte budget for one channel (reasoning or visible text) of one failed
/// turn. Once the cap is reached the budget is spent: later text in that
/// channel is dropped rather than partially interleaved.
struct RecoveryBudget {
    cap: usize,
    used: usize,
    truncated: bool,
}

impl RecoveryBudget {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            used: 0,
            truncated: false,
        }
    }

    fn append(&mut self, slot: &mut String, text: &str) {
        if self.truncated {
            return;
        }
        let remaining = self.cap.saturating_sub(self.used);
        if text.len() <= remaining {
            slot.push_str(text);
            self.used += text.len();
            return;
        }
        let mut cut = remaining;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        self.truncated = true;
        if cut == 0 && slot.is_empty() {
            // Nothing of this text fits and the slot holds nothing to mark;
            // a marker alone would read as retained content.
            return;
        }
        slot.push_str(&text[..cut]);
        slot.push_str(TRUNCATION_MARKER);
        self.used += cut;
    }

    /// Replace a slot's delta-accumulated text with the `done` value. A spent
    /// budget keeps what it already recorded instead of dropping it.
    fn replace(&mut self, slot: &mut String, text: String) {
        if self.truncated {
            return;
        }
        self.used = self.used.saturating_sub(slot.len());
        slot.clear();
        self.append(slot, &text);
    }

    /// Charge opaque bytes (an encrypted reasoning blob) that cannot be
    /// truncated. Returns `false` when they do not fit, in which case the
    /// caller drops them; the budget is left for the text that can be cut.
    fn charge(&mut self, len: usize) -> bool {
        if self.truncated || len > self.cap.saturating_sub(self.used) {
            return false;
        }
        self.used += len;
        true
    }

    fn fit(&mut self, text: &str) -> String {
        let mut out = String::new();
        self.append(&mut out, text);
        out
    }
}

/// One completed item of the failed turn, as the wire delivered it.
enum CapturedItem {
    Reasoning(Box<rs::ReasoningItem>),
    Text(String),
}

/// What one failed attempt produced, drained by the retry loop.
struct CapturedTurn {
    /// Completed wire items in `output_index` order.
    typed: Vec<CapturedItem>,
    /// Whether the turn reached its terminal frame, whose `output` list is
    /// complete — nothing may be added to it from the deltas.
    terminal_seen: bool,
    /// Streamed reasoning text per item id, for items the wire never
    /// completed.
    reasoning: Vec<(String, String)>,
    output: String,
    veto_replay: bool,
}

#[derive(Clone, Default)]
pub(crate) struct FailedResponseCapture {
    inner: Option<Arc<Mutex<CapturedResponse>>>,
}

struct CapturedResponse {
    typed: BTreeMap<u32, CapturedItem>,
    terminal_seen: bool,
    reasoning_content: BTreeMap<(u32, String), BTreeMap<u32, String>>,
    reasoning_summary: BTreeMap<(u32, String), BTreeMap<u32, String>>,
    raw_reasoning_indexes: BTreeSet<u32>,
    output: BTreeMap<(u32, u32, String), String>,
    reasoning_budget: RecoveryBudget,
    text_budget: RecoveryBudget,
    veto_replay: bool,
}

impl Default for CapturedResponse {
    fn default() -> Self {
        Self {
            typed: BTreeMap::new(),
            terminal_seen: false,
            reasoning_content: BTreeMap::new(),
            reasoning_summary: BTreeMap::new(),
            raw_reasoning_indexes: BTreeSet::new(),
            output: BTreeMap::new(),
            reasoning_budget: RecoveryBudget::new(MAX_RECOVERY_REASONING_BYTES),
            text_budget: RecoveryBudget::new(MAX_RECOVERY_TEXT_BYTES),
            veto_replay: false,
        }
    }
}

impl FailedResponseCapture {
    /// Allocate a capture for an attempt whose doom-loop abort is armed.
    pub(crate) fn armed() -> Self {
        Self {
            inner: Some(Arc::new(Mutex::new(CapturedResponse::default()))),
        }
    }

    /// Whether this capture records anything. The stream checks it before
    /// doing per-frame work for an attempt that can never be replayed.
    pub(crate) fn is_armed(&self) -> bool {
        self.inner.is_some()
    }

    /// Run `f` against the capture. Returns `None` when the capture is
    /// disarmed or its lock is poisoned — a failed capture degrades to a
    /// reminder-only retry rather than failing the request.
    fn with<R>(&self, f: impl FnOnce(&mut CapturedResponse) -> R) -> Option<R> {
        let mut captured = self.inner.as_ref()?.lock().ok()?;
        Some(f(&mut captured))
    }

    pub(crate) fn record_reasoning_delta(
        &self,
        output_index: u32,
        content_index: u32,
        item_id: String,
        delta: &str,
    ) {
        if delta.is_empty() {
            return;
        }
        self.with(|captured| {
            let slot = captured
                .reasoning_content
                .entry((output_index, item_id))
                .or_default()
                .entry(content_index)
                .or_default();
            captured.reasoning_budget.append(slot, delta);
            let retained = !slot.is_empty();
            // The raw channel only wins once it actually holds text: an empty
            // event, or one the budget dropped, must not discard the summary.
            if retained {
                captured.raw_reasoning_indexes.insert(output_index);
            }
        });
    }

    pub(crate) fn record_reasoning_done(
        &self,
        output_index: u32,
        content_index: u32,
        item_id: String,
        text: String,
    ) {
        if text.is_empty() {
            return;
        }
        self.with(|captured| {
            let slot = captured
                .reasoning_content
                .entry((output_index, item_id))
                .or_default()
                .entry(content_index)
                .or_default();
            captured.reasoning_budget.replace(slot, text);
            let retained = !slot.is_empty();
            if retained {
                captured.raw_reasoning_indexes.insert(output_index);
            }
        });
    }

    pub(crate) fn record_reasoning_summary_delta(
        &self,
        output_index: u32,
        summary_index: u32,
        item_id: String,
        delta: &str,
    ) {
        if delta.is_empty() {
            return;
        }
        self.with(|captured| {
            if captured.raw_reasoning_indexes.contains(&output_index) {
                return;
            }
            let slot = captured
                .reasoning_summary
                .entry((output_index, item_id))
                .or_default()
                .entry(summary_index)
                .or_default();
            captured.reasoning_budget.append(slot, delta);
        });
    }

    pub(crate) fn record_reasoning_summary_done(
        &self,
        output_index: u32,
        summary_index: u32,
        item_id: String,
        text: String,
    ) {
        if text.is_empty() {
            return;
        }
        self.with(|captured| {
            if captured.raw_reasoning_indexes.contains(&output_index) {
                return;
            }
            let slot = captured
                .reasoning_summary
                .entry((output_index, item_id))
                .or_default()
                .entry(summary_index)
                .or_default();
            captured.reasoning_budget.replace(slot, text);
        });
    }

    pub(crate) fn record_output_delta(
        &self,
        output_index: u32,
        content_index: u32,
        item_id: String,
        delta: &str,
    ) {
        if delta.is_empty() {
            return;
        }
        self.with(|captured| {
            let slot = captured
                .output
                .entry((output_index, content_index, item_id))
                .or_default();
            captured.text_budget.append(slot, delta);
        });
    }

    pub(crate) fn record_output_done(
        &self,
        output_index: u32,
        content_index: u32,
        item_id: String,
        text: String,
    ) {
        if text.is_empty() {
            return;
        }
        self.with(|captured| {
            let slot = captured
                .output
                .entry((output_index, content_index, item_id))
                .or_default();
            captured.text_budget.replace(slot, text);
        });
    }

    /// Give up on replaying this turn. A tool call binds the reasoning that
    /// precedes it, and a compaction item is opaque state the retry cannot
    /// carry: either way the failed turn cannot be resent piecemeal, so the
    /// retry falls back to the reminder alone.
    pub(crate) fn record_unreplayable(&self) {
        self.with(|captured| captured.veto_replay = true);
    }

    /// Record a *started* wire item, whose payload is not final yet: its kind
    /// is enough to veto a replay when it is a tool call or compaction state.
    pub(crate) fn record_item_start(&self, item: &rs::OutputItem) {
        if !matches!(
            item,
            rs::OutputItem::Message(_) | rs::OutputItem::Reasoning(_)
        ) {
            self.record_unreplayable();
        }
    }

    /// Record one completed wire item. Only messages and reasoning can be
    /// replayed; everything else — the MCP calls the conversation form drops
    /// and the opaque compaction checkpoint alike — vetoes the whole replay.
    pub(crate) fn record_output_item(&self, output_index: u32, item: &rs::OutputItem) {
        self.with(|captured| match item {
            rs::OutputItem::Reasoning(reasoning) => {
                captured.typed.insert(
                    output_index,
                    CapturedItem::Reasoning(Box::new(reasoning.clone())),
                );
            }
            rs::OutputItem::Message(message) => {
                let text = output_message_text(message);
                if !text.is_empty() {
                    captured
                        .typed
                        .insert(output_index, CapturedItem::Text(text));
                }
            }
            _ => captured.veto_replay = true,
        });
    }

    /// Record the terminal `output` list, which supersedes anything recorded
    /// item by item: it is the turn's authoritative content and order.
    pub(crate) fn record_terminal_output(&self, output: &[rs::OutputItem]) {
        if self.inner.is_none() {
            return;
        }
        self.with(|captured| {
            captured.typed.clear();
            captured.terminal_seen = true;
        });
        for (output_index, item) in output.iter().enumerate() {
            self.record_output_item(output_index as u32, item);
        }
    }

    fn take(&self) -> CapturedTurn {
        let Some(captured) = self.with(std::mem::take) else {
            return CapturedTurn {
                typed: Vec::new(),
                terminal_seen: false,
                reasoning: Vec::new(),
                output: String::new(),
                veto_replay: false,
            };
        };

        // The raw channel replaces the summary only where it actually holds
        // text, so an empty or budget-dropped raw event leaves the summary as
        // the recovery context rather than emptying it.
        let raw_content: BTreeMap<(u32, String), BTreeMap<u32, String>> = captured
            .reasoning_content
            .into_iter()
            .filter(|(_, parts)| parts.values().any(|text| !text.is_empty()))
            .collect();
        let raw_indexes: BTreeSet<u32> = raw_content
            .keys()
            .map(|(output_index, _)| *output_index)
            .collect();
        let mut reasoning_parts = captured.reasoning_summary;
        reasoning_parts.retain(|(output_index, _), _| !raw_indexes.contains(output_index));
        reasoning_parts.extend(raw_content);

        let reasoning = reasoning_parts
            .into_iter()
            .map(|((_output_index, item_id), parts)| {
                (item_id, parts.into_values().collect::<String>())
            })
            .filter(|(_, text)| !text.is_empty())
            .collect();

        CapturedTurn {
            typed: captured.typed.into_values().collect(),
            terminal_seen: captured.terminal_seen,
            reasoning,
            output: captured.output.into_values().collect(),
            veto_replay: captured.veto_replay,
        }
    }

    /// The failed turn as the retry should replay it: completed wire items
    /// where the turn produced them, streamed deltas only for what the wire
    /// never completed.
    pub(crate) fn take_items(&self) -> Vec<ConversationItem> {
        let captured = self.take();
        if captured.veto_replay {
            return Vec::new();
        }

        // A turn that reached its terminal frame has told us everything it
        // produced: the deltas may not add items the wire left out, not even
        // when the terminal projection comes out empty.
        let wire_is_complete = captured.terminal_seen;
        let mut reasoning_budget = RecoveryBudget::new(MAX_RECOVERY_REASONING_BYTES);
        let mut text_budget = RecoveryBudget::new(MAX_RECOVERY_TEXT_BYTES);
        let mut streamed = captured.reasoning;
        let mut items: Vec<ConversationItem> = Vec::with_capacity(captured.typed.len() + 1);
        let mut wire_carried_text = false;
        for item in captured.typed {
            match item {
                CapturedItem::Reasoning(reasoning) => {
                    let mut reasoning = *reasoning;
                    let streamed_text = take_streamed(&mut streamed, &reasoning.id);
                    fit_reasoning(&mut reasoning, streamed_text, &mut reasoning_budget);
                    if reasoning_has_text(&reasoning) || reasoning.encrypted_content.is_some() {
                        items.push(ConversationItem::Reasoning(reasoning));
                    }
                }
                CapturedItem::Text(text) => {
                    wire_carried_text = true;
                    let text = text_budget.fit(&text);
                    if !text.is_empty() {
                        items.push(ConversationItem::assistant(text));
                    }
                }
            }
        }

        if wire_is_complete {
            return items;
        }

        // Reasoning the wire never completed (the attempt was aborted
        // mid-item) is replayed from the deltas, after the completed items.
        for (item_id, text) in streamed {
            let text = reasoning_budget.fit(&text);
            if text.is_empty() {
                continue;
            }
            items.push(ConversationItem::Reasoning(rs::ReasoningItem {
                id: item_id,
                summary: Vec::new(),
                content: Some(vec![rs::ReasoningTextContent { text }]),
                encrypted_content: None,
                status: None,
            }));
        }
        if !wire_carried_text {
            let text = text_budget.fit(&captured.output);
            if !text.is_empty() {
                items.push(ConversationItem::assistant(text));
            }
        }
        items
    }
}

/// Visible text of a completed message item, refusals included: a refusal is
/// still model-authored output the retry should see.
fn output_message_text(message: &rs::OutputMessage) -> String {
    message
        .content
        .iter()
        .map(|part| match part {
            rs::OutputMessageContent::OutputText(text) => text.text.as_str(),
            rs::OutputMessageContent::Refusal(refusal) => refusal.refusal.as_str(),
        })
        .collect()
}

fn take_streamed(streamed: &mut Vec<(String, String)>, item_id: &str) -> Option<String> {
    let at = streamed.iter().position(|(id, _)| id == item_id)?;
    Some(streamed.remove(at).1)
}

/// Cap a final reasoning item against the turn budget, filling empty content
/// from the streamed text when the final item carried none. The opaque
/// `encrypted_content` blob is charged to the same budget and dropped when it
/// does not fit, so it cannot smuggle an unbounded turn into the retry.
fn fit_reasoning(
    reasoning: &mut rs::ReasoningItem,
    streamed_text: Option<String>,
    budget: &mut RecoveryBudget,
) {
    let typed_content: Vec<String> = reasoning
        .content
        .take()
        .unwrap_or_default()
        .into_iter()
        .map(|part| part.text)
        .filter(|text| !text.is_empty())
        .collect();
    let content = if typed_content.is_empty() {
        streamed_text.into_iter().collect()
    } else {
        typed_content
    };
    let content: Vec<rs::ReasoningTextContent> = content
        .into_iter()
        .map(|text| rs::ReasoningTextContent {
            text: budget.fit(&text),
        })
        .filter(|part| !part.text.is_empty())
        .collect();
    reasoning.content = (!content.is_empty()).then_some(content);

    for part in &mut reasoning.summary {
        let rs::SummaryPart::SummaryText(summary) = part;
        summary.text = budget.fit(&summary.text);
    }
    reasoning
        .summary
        .retain(|rs::SummaryPart::SummaryText(summary)| !summary.text.is_empty());

    if let Some(encrypted) = &reasoning.encrypted_content
        && !budget.charge(encrypted.len())
    {
        reasoning.encrypted_content = None;
    }
}

fn reasoning_has_text(reasoning: &rs::ReasoningItem) -> bool {
    reasoning
        .content
        .as_ref()
        .is_some_and(|parts| parts.iter().any(|part| !part.text.is_empty()))
        || !reasoning.summary.is_empty()
}

pub(crate) fn append_recovery_context(
    request: &mut ConversationRequest,
    failed_items: Vec<ConversationItem>,
) {
    request.items.extend(failed_items);
    request
        .items
        .push(ConversationItem::system_reminder(RECOVERY_REMINDER));
}

#[cfg(test)]
#[path = "doom_loop_recovery_tests.rs"]
mod tests;
