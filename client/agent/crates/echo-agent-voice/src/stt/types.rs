use serde::Deserialize;

/// Parsed server → client STT WebSocket events.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SttServerEvent {
    #[serde(rename = "transcript.created")]
    Created {},
    #[serde(rename = "transcript.partial")]
    Partial {
        #[serde(default)]
        text: String,
        #[serde(default)]
        is_final: bool,
        #[serde(default)]
        speech_final: bool,
    },
    #[serde(rename = "transcript.done")]
    Done {
        #[serde(default)]
        text: String,
        #[serde(default)]
        duration: Option<f32>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Unknown,
}

/// Normalized partial transcript for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttTranscriptPartial {
    pub text: String,
    pub is_final: bool,
    pub speech_final: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_partial_event() {
        let raw =
            r#"{"type":"transcript.partial","text":"hello","is_final":false,"speech_final":false}"#;
        let ev: SttServerEvent = serde_json::from_str(raw).unwrap();
        let SttServerEvent::Partial {
            text, speech_final, ..
        } = ev
        else {
            panic!("expected partial");
        };
        assert_eq!(text, "hello");
        assert!(!speech_final);
    }

    #[test]
    fn parse_speech_final() {
        let raw =
            r#"{"type":"transcript.partial","text":"done","is_final":true,"speech_final":true}"#;
        let ev: SttServerEvent = serde_json::from_str(raw).unwrap();
        let SttServerEvent::Partial { speech_final, .. } = ev else {
            panic!("expected partial");
        };
        assert!(speech_final);
    }
}
