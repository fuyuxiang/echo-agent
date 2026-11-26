//! The `--include-partial-messages` stream framing for `streaming-messages-json`:
//! the raw Messages API `stream_event` mechanics and their interaction with the
//! typed [`PartialFraming`] state. Only reachable when partial messages are on.

use serde_json::{Value, json};

use crate::headless::reducer::to_line;

use super::MessagesReducer;
use super::state::{OpenBlock, PartialFraming, TextKind};
use super::wire::{
    EmptyObject, MessageDeltaBody, MessagesLine, PartialBlock, PartialDelta, PartialEventLine,
    PartialMessage, StreamEventBody, new_uuid,
};

impl MessagesReducer {
    /// Wrap a raw Messages API stream event in a `stream_event` line.
    fn partial_wrap(&self, event: StreamEventBody) -> Value {
        to_line(&MessagesLine::StreamEvent(PartialEventLine {
            event: to_line(&event),
            parent_tool_use_id: None,
            session_id: self.session_id().to_string(),
            uuid: new_uuid(),
        }))
    }

    /// Open the partial `message_start` on first use, carrying the real id, model,
    /// and input-side usage (or a synthesized id and zero usage when absent).
    fn partial_open_message(&mut self, out: &mut Vec<Value>) {
        if self.framing.message_open() {
            return;
        }
        // Clone (never move out) so the real values remain for the final frame.
        let identity = self.response.identity();
        let id = identity.message_id.clone().unwrap_or_else(|| {
            let id = format!("msg_{}", self.partial_msg_seq);
            self.partial_msg_seq += 1;
            id
        });
        let model = self.frame_model(&identity);
        out.push(self.partial_wrap(StreamEventBody::MessageStart {
            message: PartialMessage {
                id,
                kind: "message",
                role: "assistant",
                model,
                content: Vec::new(),
                stop_reason: None,
                stop_sequence: None,
                usage: identity.input_usage(),
            },
        }));
        self.framing = PartialFraming::MessageOpen { block: None };
    }

    /// Emit the partial framing for a text/thinking delta at `index`, opening the
    /// message and content block on first use.
    pub(super) fn partial_delta(
        &mut self,
        out: &mut Vec<Value>,
        kind: TextKind,
        index: usize,
        delta: PartialDelta,
    ) {
        self.partial_open_message(out);
        let open = self.framing.open_block().unwrap_or_else(|| {
            let content_block = match kind {
                TextKind::Text => PartialBlock::Text { text: "" },
                TextKind::Thinking => PartialBlock::Thinking {
                    thinking: "",
                    signature: "",
                },
            };
            out.push(self.partial_wrap(StreamEventBody::ContentBlockStart {
                index,
                content_block,
            }));
            let block = OpenBlock { index, kind };
            self.framing = PartialFraming::MessageOpen { block: Some(block) };
            block
        });
        // Target the open block's own index, not the caller's, so a delta cannot drift.
        out.push(self.partial_wrap(StreamEventBody::ContentBlockDelta {
            index: open.index,
            delta,
        }));
    }

    /// Emit a full `tool_use` content block in the partial stream (one `input_json_delta`).
    pub(super) fn partial_tool_use(
        &mut self,
        out: &mut Vec<Value>,
        index: usize,
        id: &str,
        name: &str,
        input: &Value,
    ) {
        self.partial_open_message(out);
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStart {
            index,
            content_block: PartialBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: EmptyObject {},
            },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockDelta {
            index,
            delta: PartialDelta::InputJson {
                partial_json: input.to_string(),
            },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStop { index }));
    }

    /// Emit the partial framing for a `server_tool_use` block (start, `input_json_delta`, stop).
    pub(super) fn partial_server_tool_use(
        &mut self,
        out: &mut Vec<Value>,
        index: usize,
        id: &str,
        query: &str,
    ) {
        self.partial_open_message(out);
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStart {
            index,
            content_block: PartialBlock::ServerToolUse {
                id: id.to_string(),
                name: "web_search",
                input: EmptyObject {},
            },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockDelta {
            index,
            delta: PartialDelta::InputJson {
                partial_json: json!({ "query": query }).to_string(),
            },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStop { index }));
    }

    /// Emit the partial framing for a `web_search_tool_result` block (hits ride `content_block_start`).
    pub(super) fn partial_web_search_result(
        &mut self,
        out: &mut Vec<Value>,
        index: usize,
        tool_use_id: &str,
        hits: &Value,
    ) {
        self.partial_open_message(out);
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStart {
            index,
            content_block: PartialBlock::WebSearchToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: hits.clone(),
            },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStop { index }));
    }

    /// Emit the partial framing for a signature-only thinking block (start + `signature_delta` + stop).
    /// The signature is cloned (not taken) so `finalize_open` materializes the same block once.
    pub(super) fn partial_signature_only_block(&mut self, out: &mut Vec<Value>) {
        if !self.include_partials()
            || self.open_kind.is_some()
            || self.framing.open_block().is_some()
        {
            return;
        }
        let Some(signature) = self.open_signature.clone() else {
            return;
        };
        self.partial_open_message(out);
        let index = self.blocks.len();
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStart {
            index,
            content_block: PartialBlock::Thinking {
                thinking: "",
                signature: "",
            },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockDelta {
            index,
            delta: PartialDelta::Signature { signature },
        }));
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStop { index }));
    }

    /// Close the open content block. A thinking block emits `signature_delta` first
    /// when its signature is known; cloned (not taken) so `finalize_open` can reuse it.
    pub(super) fn partial_close_block(&mut self, out: &mut Vec<Value>) {
        let Some(block) = self.framing.open_block() else {
            return;
        };
        let index = block.index;
        if block.kind == TextKind::Thinking {
            let sig = self
                .open_signature
                .clone()
                .or_else(|| self.response.pending().and_then(|p| p.signature.clone()));
            if let Some(sig) = sig {
                out.push(self.partial_wrap(StreamEventBody::ContentBlockDelta {
                    index,
                    delta: PartialDelta::Signature { signature: sig },
                }));
            }
        }
        out.push(self.partial_wrap(StreamEventBody::ContentBlockStop { index }));
        self.framing = PartialFraming::MessageOpen { block: None };
    }

    /// Close the open message framing before a frame is flushed. `default_stop_reason`
    /// must match `flush_assistant`'s so the partial rebuild and frame never disagree.
    pub(super) fn partial_close_message(
        &mut self,
        out: &mut Vec<Value>,
        default_stop_reason: Option<&str>,
    ) {
        if !self.include_partials() {
            return;
        }
        self.partial_close_block(out);
        self.partial_signature_only_block(out);
        if !self.framing.message_open() && self.response.started() {
            self.partial_open_message(out);
        }
        if self.framing.message_open() {
            let stop_reason = self.resolved_stop_reason(default_stop_reason);
            let usage = self.resolved_usage();
            let stop_sequence = self.resolved_stop_sequence();
            out.push(self.partial_wrap(StreamEventBody::MessageDelta {
                delta: MessageDeltaBody {
                    stop_reason,
                    stop_sequence,
                },
                usage,
            }));
            out.push(self.partial_wrap(StreamEventBody::MessageStop));
            self.framing = PartialFraming::Idle;
        }
    }
}
