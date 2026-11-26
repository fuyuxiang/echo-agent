//! Pure text-classification helpers shared by the sibling [`crate::flush`]
//! and [`crate::dream`] response-processing modules.
//!
//! Keeping these helpers separate lets both memory domains depend on
//! `text_utils` without depending on each other.

/// Check if text contains at least one markdown header (`#` or `##`).
///
/// Used by both flush and dream response processing to ensure the model
/// produced structured output.
pub fn has_markdown_headers(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
        (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ')
    })
}

/// Remove provider reasoning envelopes before model output is persisted.
///
/// Some OpenAI-compatible gateways serialize hidden reasoning as literal
/// `<think>...</think>` text. Persisting that text leaks implementation detail
/// into memory and can also make a trailing `NO_REPLY` look like valid markdown.
/// An unterminated reasoning block is discarded through the end of the response:
/// incomplete hidden reasoning is never useful durable memory.
pub fn strip_reasoning_blocks(text: &str) -> String {
    let mut filter = PublicText::default();
    let mut output = filter.push(text);
    output.push_str(&filter.finish());
    output
}

/// Incremental public-output boundary. Never retains hidden text; only a small
/// candidate tag and Markdown delimiter state survive a chunk or disk restore.
/// Code examples in fenced/inline code are literal, including reasoning tags.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PublicText {
    hidden: Vec<String>,
    pending: String,
    marker: String,
    code: Option<(char, usize, bool)>,
    line_prefix: bool,
    #[serde(default)]
    indent: usize,
    #[serde(default)]
    indented_line: bool,
    #[serde(default)]
    fence_close: bool,
    #[serde(default)]
    opening_fence: bool,
    #[serde(default)]
    discard: bool,
}

impl Default for PublicText {
    fn default() -> Self {
        Self {
            hidden: vec![],
            pending: String::new(),
            marker: String::new(),
            code: None,
            line_prefix: true,
            indent: 0,
            indented_line: false,
            fence_close: false,
            opening_fence: false,
            discard: false,
        }
    }
}

impl PublicText {
    fn emit(&mut self, value: &str, output: &mut String) {
        if !self.hidden.is_empty() {
            return;
        }
        output.push_str(value);
        // Markdown position belongs to public text. Erased reasoning must not
        // turn a leading code fence into an inline span or alter indentation.
        for ch in value.chars() {
            if ch == '\n' {
                self.line_prefix = true;
                self.indent = 0;
                self.indented_line = false;
                self.opening_fence = false;
            } else if self.line_prefix && (ch == ' ' || ch == '\t') {
                self.indent += if ch == '\t' { 4 - self.indent % 4 } else { 1 };
            } else if ch != '\r' {
                if self.line_prefix && self.indent >= 4 {
                    self.indented_line = true;
                }
                self.line_prefix = false;
            }
        }
    }

    fn flush_marker(&mut self, output: &mut String) {
        if self.marker.is_empty() {
            return;
        }
        self.fence_close = false;
        let marker = std::mem::take(&mut self.marker);
        let ch = marker.chars().next().unwrap();
        let len = marker.len();
        match self.code {
            Some((c, n, true)) if self.opening_fence && c == '`' && ch == c => {
                // Backticks cannot occur in a backtick fence's info string.
                // This is instead an inline code span on the same line.
                self.code = if len == n { None } else { Some((c, n, false)) };
                self.opening_fence = false;
            }
            Some((c, n, true)) if c == ch && self.line_prefix && self.indent <= 3 && len >= n => {
                // Keep treating text as code until the rest of this line is
                // known to contain only whitespace. A delimiter plus info is
                // literal code, not a closing fence.
                self.fence_close = true;
            }
            Some((c, n, false)) if c == ch && len == n => self.code = None,
            None if self.line_prefix && self.indent <= 3 && len >= 3 => {
                self.code = Some((ch, len, true));
                self.opening_fence = true;
            }
            None if ch == '`' => self.code = Some((ch, len, false)),
            _ => {}
        }
        self.emit(&marker, output);
    }

    pub fn push(&mut self, text: &str) -> String {
        if self.discard {
            return String::new();
        }
        let mut output = String::new();
        for ch in text.chars() {
            if !self.pending.is_empty() {
                self.pending.push(ch);
                let lower = self.pending.to_ascii_lowercase();
                let body = lower.trim_start_matches('<');
                let closing = body.starts_with('/');
                let body = body.strip_prefix('/').unwrap_or(body);
                let tag = ["think", "thinking", "reasoning", "analysis"]
                    .into_iter()
                    .find(|tag| {
                        body.starts_with(tag)
                            && body[tag.len()..]
                                .starts_with(|c: char| c == '>' || c.is_ascii_whitespace())
                    });
                if self.pending.len() > 256 && tag.is_some() {
                    self.pending.clear();
                    self.discard = true;
                    break;
                } else if ch == '>' && tag.is_some() {
                    let tag = tag.unwrap();
                    if closing {
                        if self.hidden.last().is_some_and(|value| value == tag) {
                            self.hidden.pop();
                        }
                    } else {
                        if self.hidden.len() == 32 {
                            self.pending.clear();
                            self.discard = true;
                            break;
                        }
                        self.hidden.push(tag.into());
                    }
                    self.pending.clear();
                } else if self.pending.len() > 256
                    || !(tag.is_some()
                        || ["think", "thinking", "reasoning", "analysis"]
                            .iter()
                            .any(|tag| tag.starts_with(body)))
                {
                    let pending = std::mem::take(&mut self.pending);
                    self.emit(&pending, &mut output);
                }
                continue;
            }
            if self.hidden.is_empty() && (ch == '`' || ch == '~') {
                if self.marker.starts_with(|c| c != ch) {
                    self.flush_marker(&mut output);
                }
                self.marker.push(ch);
                continue;
            }
            self.flush_marker(&mut output);
            if self.fence_close {
                if ch == '\n' {
                    self.code = None;
                    self.fence_close = false;
                } else if !matches!(ch, ' ' | '\t' | '\r') {
                    self.fence_close = false;
                }
            }
            let indented = self.indented_line || (self.line_prefix && self.indent >= 4);
            if ch == '<' && ((self.code.is_none() && !indented) || !self.hidden.is_empty()) {
                self.pending.push(ch);
            } else {
                self.emit(&ch.to_string(), &mut output);
            }
        }
        output
    }

    pub fn finish(&mut self) -> String {
        let mut output = String::new();
        self.flush_marker(&mut output);
        if self.fence_close {
            self.code = None;
            self.fence_close = false;
        }
        // An incomplete tag or hidden block never becomes public on completion.
        self.pending.clear();
        output
    }
}

/// Check if the response matches the NO_REPLY convention.
///
/// Strips all non-alphanumeric characters, lowercases, and checks if the
/// remainder is exactly `"noreply"`. This handles common separator variants:
/// `"no reply"`, `"no_reply"`, `"no-reply"`, `"NO REPLY"`, etc.
pub fn is_no_reply(text: &str) -> bool {
    let normalized: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    normalized == "noreply"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_no_reply() {
        assert!(is_no_reply("NO_REPLY"));
        assert!(is_no_reply("no reply"));
        assert!(is_no_reply("No-Reply"));
        assert!(is_no_reply("noreply"));
        assert!(!is_no_reply("no reply needed"));
        assert!(!is_no_reply("I have things to store"));
    }

    #[test]
    fn test_has_markdown_headers() {
        assert!(has_markdown_headers("## Topic"));
        assert!(has_markdown_headers("# Title\n\nBody"));
        assert!(has_markdown_headers("preamble\n\n## Topic"));
        assert!(!has_markdown_headers("plain text without headers"));
        assert!(!has_markdown_headers("#hashtag without space"));
        assert!(!has_markdown_headers("inline ## heading-like text"));
    }

    #[test]
    fn test_strip_reasoning_blocks() {
        assert_eq!(
            strip_reasoning_blocks("<think>private ## notes</think>\n## Useful\nFact"),
            "\n## Useful\nFact"
        );
        assert_eq!(
            strip_reasoning_blocks("before<THINK>hidden</THINK>after"),
            "beforeafter"
        );
        assert_eq!(strip_reasoning_blocks("<thinking>unfinished"), "");
        assert_eq!(strip_reasoning_blocks("## untouched"), "## untouched");
    }
}
