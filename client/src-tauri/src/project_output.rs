//! Bounded public output for explicitly shared project executions only.
use echo_agent_memory::text_utils::PublicText;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const MAX_OUTPUT_CHARS: usize = 12_000;
const MAX_RUNS: usize = 128;

#[derive(Default)]
struct Capture {
    filter: PublicText,
    text: String,
    events: VecDeque<String>,
}

impl Capture {
    fn append(&mut self, text: &str) {
        self.text.push_str(text);
        let excess = self.text.chars().count().saturating_sub(MAX_OUTPUT_CHARS);
        if excess > 0 {
            let offset = self
                .text
                .char_indices()
                .nth(excess)
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.drain(..offset);
        }
    }
}

#[derive(Default)]
struct Outputs {
    runs: HashMap<(String, String), Capture>,
    order: VecDeque<(String, String)>,
}

impl Outputs {
    fn start(&mut self, session: &str, run: &str) {
        let key = (session.to_owned(), run.to_owned());
        if self.runs.contains_key(&key) {
            return;
        }
        while self.order.len() >= MAX_RUNS {
            if let Some(oldest) = self.order.pop_front() {
                self.runs.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.runs.insert(key, Capture::default());
    }

    fn observe(&mut self, session: &str, update: &Value, meta: Option<&Value>) {
        if update.get("sessionUpdate").and_then(Value::as_str) != Some("agent_message_chunk")
            || meta
                .and_then(|m| m.get("isReplay").or_else(|| m.get("replay")))
                .and_then(Value::as_bool)
                == Some(true)
        {
            return;
        }
        let Some(text) = update.pointer("/content/text").and_then(Value::as_str) else {
            return;
        };
        let prompt = meta
            .and_then(|m| m.get("promptId").or_else(|| m.get("prompt_id")))
            .or_else(|| update.get("promptId").or_else(|| update.get("prompt_id")))
            .and_then(Value::as_str);
        // Ambiguous or old prompt events never enter another execution's record.
        let mut matches = self
            .runs
            .keys()
            .filter(|(sid, run)| sid == session && prompt.is_none_or(|id| id == run));
        let Some(key) = matches.next().cloned() else {
            return;
        };
        if matches.next().is_some() {
            return;
        }
        let capture = self.runs.get_mut(&key).unwrap();
        if let Some(event) = meta.and_then(|m| m.get("eventId")).and_then(Value::as_str) {
            if capture.events.iter().any(|id| id == event) {
                return;
            }
            capture.events.push_back(event.to_owned());
            if capture.events.len() > 256 {
                capture.events.pop_front();
            }
        }
        let public = capture.filter.push(text);
        capture.append(&public);
    }

    fn finish(&mut self, session: &str, run: &str) -> Option<String> {
        let key = (session.to_owned(), run.to_owned());
        self.order.retain(|entry| entry != &key);
        let mut capture = self.runs.remove(&key)?;
        let tail = capture.filter.finish();
        capture.append(&tail);
        let text = capture.text.trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

static OUTPUTS: OnceLock<Mutex<Outputs>> = OnceLock::new();
pub(crate) fn start(session: &str, run: &str) {
    OUTPUTS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .start(session, run);
}
pub(crate) fn observe(session: &str, update: &Value, meta: Option<&Value>) {
    if let Some(outputs) = OUTPUTS.get() {
        outputs.lock().unwrap().observe(session, update, meta);
    }
}
pub(crate) fn finish(session: &str, run: &str) -> Option<String> {
    OUTPUTS.get()?.lock().unwrap().finish(session, run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn chunk(text: &str) -> Value {
        json!({"sessionUpdate":"agent_message_chunk","content":{"text":text}})
    }

    #[test]
    fn project_output_filters_hidden_text_replays_and_duplicate_events() {
        let mut output = Outputs::default();
        output.start("session", "run");
        output.observe(
            "session",
            &chunk("<thi"),
            Some(&json!({"promptId":"run","eventId":"1"})),
        );
        output.observe(
            "session",
            &chunk("nk>private</think>公开结论"),
            Some(&json!({"promptId":"run","eventId":"2"})),
        );
        output.observe(
            "session",
            &chunk("duplicate"),
            Some(&json!({"promptId":"run","eventId":"2"})),
        );
        output.observe(
            "session",
            &chunk("replay"),
            Some(&json!({"promptId":"run","isReplay":true})),
        );
        output.observe(
            "session",
            &json!({"sessionUpdate":"agent_thought_chunk","content":{"text":"secret"}}),
            None,
        );
        assert_eq!(output.finish("session", "run").as_deref(), Some("公开结论"));
        assert!(output.finish("session", "run").is_none());
    }

    #[test]
    fn project_output_fences_overlapping_prompts_and_unbound_sessions() {
        let mut output = Outputs::default();
        output.start("session", "old");
        output.start("session", "new");
        output.observe("session", &chunk("ambiguous"), None);
        output.observe("private", &chunk("private chat"), None);
        output.observe(
            "session",
            &chunk("old result"),
            Some(&json!({"promptId":"old"})),
        );
        output.observe(
            "session",
            &chunk("new result"),
            Some(&json!({"promptId":"new"})),
        );
        assert_eq!(
            output.finish("session", "old").as_deref(),
            Some("old result")
        );
        assert_eq!(
            output.finish("session", "new").as_deref(),
            Some("new result")
        );
        assert!(output.runs.is_empty());
    }

    #[test]
    fn project_output_keeps_bounded_unicode_tail() {
        let mut output = Outputs::default();
        output.start("session", "run");
        output.observe("session", &chunk(&"中".repeat(20_000)), None);
        output.observe("session", &chunk("最终结果"), None);
        let text = output.finish("session", "run").unwrap();
        assert_eq!(text.chars().count(), MAX_OUTPUT_CHARS);
        assert!(text.ends_with("最终结果"));
        for i in 0..MAX_RUNS + 10 {
            output.start("s", &i.to_string());
        }
        assert_eq!(output.runs.len(), MAX_RUNS);
    }
}
