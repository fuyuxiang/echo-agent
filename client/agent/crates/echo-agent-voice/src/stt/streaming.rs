use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::{Error as WsError, ProtocolError};
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use url::Url;

use crate::config::VoiceConfig;
use crate::error::VoiceError;
use crate::stt::types::{SttServerEvent, SttTranscriptPartial};

/// Events delivered from an active streaming STT session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamingSttEvent {
    Ready,
    Partial(SttTranscriptPartial),
    Done { text: String },
    Error { message: String },
}

/// Streaming STT over `wss://api.echo-agent.invalid/v1/stt`.
pub struct StreamingSttSession {
    audio_tx: Option<mpsc::Sender<Vec<u8>>>,
    event_rx: mpsc::Receiver<StreamingSttEvent>,
    _writer_task: JoinHandle<()>,
    _reader_task: JoinHandle<()>,
}

impl StreamingSttSession {
    /// Connect and wait for `transcript.created` before sending audio.
    pub async fn connect(config: &VoiceConfig, bearer: &str) -> Result<Self, VoiceError> {
        let url = build_stt_ws_url(config)?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| VoiceError::WebSocket(format!("request: {e}")))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {bearer}")
                .parse()
                .map_err(|e| VoiceError::WebSocket(format!("auth header: {e}")))?,
        );

        // Request-identity headers so the backend can attribute and meter voice
        // usage by client, mirroring what the sampler / imagine request paths
        // send. Billing itself follows the `Authorization` bearer (per-user for
        // OAuth, BYOK key owner otherwise); these are purely for usage
        // attribution. Skipped when empty (e.g. the probe binary / tests) or
        // when a value isn't a valid header (never fatal — the connection is
        // still fully authorized without them).
        insert_optional_header(
            &mut request,
            "x-echo-agent-client-identifier",
            &config.client_identifier,
        );
        insert_optional_header(&mut request, "User-Agent", &config.user_agent);

        // The default connector never sees the shared trust config.
        let connector =
            tokio_tungstenite::Connector::Rustls(echo_agent_extra_ca::rustls_client_config());
        let disable_nagle = false;
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                None,
                disable_nagle,
                Some(connector),
            ),
        )
        .await
        .map_err(|_| VoiceError::WebSocket("connect timed out".into()))?
        .map_err(|e| VoiceError::WebSocket(format!("connect: {e}")))?;

        let (mut ws_write, mut ws_read) = ws.split();
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(64);
        let (event_tx, event_rx) = mpsc::channel::<StreamingSttEvent>(64);

        let writer_task = tokio::spawn(async move {
            while let Some(chunk) = audio_rx.recv().await {
                if ws_write.send(Message::Binary(chunk.into())).await.is_err() {
                    break;
                }
            }
            let _ = ws_write
                .send(Message::Text(r#"{"type":"audio.done"}"#.into()))
                .await;
        });

        let reader_task = tokio::spawn(async move {
            loop {
                match ws_read.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let event = match serde_json::from_str::<SttServerEvent>(&text) {
                            Ok(SttServerEvent::Created {}) => Some(StreamingSttEvent::Ready),
                            Ok(SttServerEvent::Partial {
                                text,
                                is_final,
                                speech_final,
                            }) => Some(StreamingSttEvent::Partial(SttTranscriptPartial {
                                text,
                                is_final,
                                speech_final,
                            })),
                            Ok(SttServerEvent::Done { text, .. }) => {
                                Some(StreamingSttEvent::Done { text })
                            }
                            Ok(SttServerEvent::Error { message }) => {
                                Some(StreamingSttEvent::Error { message })
                            }
                            Ok(SttServerEvent::Unknown) => None,
                            Err(e) => Some(StreamingSttEvent::Error {
                                message: format!("parse error: {e}"),
                            }),
                        };
                        if let Some(ev) = event
                            && event_tx.send(ev).await.is_err()
                        {
                            break;
                        }
                    }
                    // Non-text frames (Close/Binary/Ping/Pong): ignore and let a
                    // subsequent `None` terminate the loop. A graceful close is
                    // treated as a normal end, not an error.
                    Some(Ok(_)) => continue,
                    // Transport-level failure: surface it so the pager can render
                    // and stop listening — except for an abrupt reset, which is
                    // also what we see when the socket is torn down at the end of
                    // a turn (incl. our own teardown), so reporting it would just
                    // produce a spurious "connection lost" toast.
                    Some(Err(e)) => {
                        if !is_benign_disconnect(&e) {
                            let _ = event_tx
                                .send(StreamingSttEvent::Error {
                                    message: format!("connection lost: {e}"),
                                })
                                .await;
                        }
                        break;
                    }
                    // Stream ended cleanly: normal end of session, no error.
                    None => break,
                }
            }
        });

        let mut session = Self {
            audio_tx: Some(audio_tx),
            event_rx,
            _writer_task: writer_task,
            _reader_task: reader_task,
        };
        session.wait_ready().await?;
        Ok(session)
    }

    async fn wait_ready(&mut self) -> Result<(), VoiceError> {
        match tokio::time::timeout(Duration::from_secs(10), self.event_rx.recv()).await {
            Ok(Some(StreamingSttEvent::Ready)) => Ok(()),
            Ok(Some(StreamingSttEvent::Error { message })) => Err(VoiceError::Stt(message)),
            Ok(_) => Err(VoiceError::Stt("unexpected event before ready".into())),
            Err(_) => Err(VoiceError::Stt(
                "timed out waiting for transcript.created".into(),
            )),
        }
    }

    pub async fn send_pcm(&self, pcm_bytes: Vec<u8>) -> Result<(), VoiceError> {
        let Some(tx) = &self.audio_tx else {
            return Err(VoiceError::Stt("audio input closed".into()));
        };
        tx.send(pcm_bytes)
            .await
            .map_err(|_| VoiceError::Stt("audio channel closed".into()))
    }

    pub async fn recv(&mut self) -> Option<StreamingSttEvent> {
        self.event_rx.recv().await
    }

    /// Stop accepting PCM; the WebSocket task sends `audio.done` when the channel closes.
    pub fn finish_audio(&mut self) {
        self.audio_tx.take();
    }

    /// Clone the live audio sender for the capture bridge.
    pub fn audio_sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.audio_tx.clone()
    }
}

impl Drop for StreamingSttSession {
    fn drop(&mut self) {
        // Dropping a `JoinHandle` only detaches the task — it does not stop it.
        // Abort both halves so an aborted setup (e.g. `connect` returning `Err`
        // after `wait_ready` fails, or the caller failing to open the mic after
        // a successful connect) tears the socket down immediately instead of
        // leaving the writer to emit a stray `audio.done` and the reader to
        // linger on an idle connection. On the healthy path both tasks have
        // already finished (audio drained, `audio.done` flushed) before drop,
        // so these aborts are no-ops.
        self._writer_task.abort();
        self._reader_task.abort();
    }
}

/// Disconnects that aren't worth surfacing to the user: a socket torn down
/// without a closing handshake is the normal result of ending a turn (the
/// client or server just drops the connection), not a real failure.
fn is_benign_disconnect(err: &WsError) -> bool {
    matches!(
        err,
        WsError::ConnectionClosed
            | WsError::AlreadyClosed
            | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)
    )
}

/// Insert `name: value` into the handshake request, unless `value` is empty
/// (header omitted) or not a valid header value (skipped with a debug log). A
/// missing identity header never fails the connection — the bearer alone fully
/// authorizes the request; these headers only enrich server-side attribution.
fn insert_optional_header(request: &mut WsRequest, name: &'static str, value: &str) {
    if value.is_empty() {
        return;
    }
    match HeaderValue::from_str(value) {
        Ok(header_value) => {
            request.headers_mut().insert(name, header_value);
        }
        Err(e) => {
            tracing::debug!(
                header = name,
                "skipping voice STT header (invalid value): {e}"
            );
        }
    }
}

fn build_stt_ws_url(config: &VoiceConfig) -> Result<Url, VoiceError> {
    // Resolve `auto` / aliases here so the wire value is always a concrete
    // catalog code (the STT API does not accept `auto`, unlike TTS).
    let language = crate::language_for_api(&config.language);
    Url::parse_with_params(
        &config.stt_ws_url()?,
        &[
            ("sample_rate", config.sample_rate.to_string()),
            ("encoding", "pcm".into()),
            ("interim_results", config.stt_interim_results.to_string()),
            ("language", language.to_string()),
            ("endpointing", config.stt_endpointing_ms.to_string()),
        ],
    )
    .map_err(|e| VoiceError::Stt(format!("bad STT URL: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_url_includes_query_params() {
        let cfg = VoiceConfig::default();
        let url = build_stt_ws_url(&cfg).unwrap();
        let q = url.query().unwrap_or_default();
        assert!(q.contains("sample_rate=16000"));
        assert!(q.contains("encoding=pcm"));
        assert!(q.contains("language=en"), "default language on wire: {q}");
    }

    #[test]
    fn stt_url_resolves_auto_to_concrete_language() {
        let cfg = VoiceConfig {
            language: "auto".into(),
            ..VoiceConfig::default()
        };
        let url = build_stt_ws_url(&cfg).unwrap();
        let q = url.query().unwrap_or_default();
        assert!(
            !q.contains("language=auto"),
            "must never send auto to STT API: {q}"
        );
        assert!(
            q.contains("language="),
            "resolved language query param missing: {q}"
        );
        // Resolved value must be a catalog code.
        let lang = q
            .split('&')
            .find_map(|p| p.strip_prefix("language="))
            .expect("language param");
        assert!(
            crate::stt_language_by_code(lang).is_some(),
            "resolved language {lang:?} not in STT catalog"
        );
    }

    #[test]
    fn stt_url_passes_through_catalog_language() {
        let cfg = VoiceConfig {
            language: "ja".into(),
            ..VoiceConfig::default()
        };
        let url = build_stt_ws_url(&cfg).unwrap();
        assert!(url.query().unwrap_or_default().contains("language=ja"));
    }

    #[test]
    fn optional_header_inserted_when_present_skipped_when_empty() {
        let mut req = "wss://api.echo-agent.invalid/v1/stt"
            .into_client_request()
            .unwrap();
        insert_optional_header(
            &mut req,
            "x-echo-agent-client-identifier",
            "echo-agent-shell",
        );
        insert_optional_header(&mut req, "User-Agent", "");
        assert_eq!(
            req.headers().get("x-echo-agent-client-identifier").unwrap(),
            "echo-agent-shell"
        );
        assert!(
            req.headers().get("user-agent").is_none(),
            "empty value must omit the header entirely"
        );
    }

    #[test]
    fn optional_header_skips_invalid_value_without_panic() {
        let mut req = "wss://api.echo-agent.invalid/v1/stt"
            .into_client_request()
            .unwrap();
        // A control char is not a valid header value; it must be dropped
        // silently, never panic or fail the (already-authorized) handshake.
        insert_optional_header(&mut req, "User-Agent", "bad\nvalue");
        assert!(
            req.headers().get("user-agent").is_none(),
            "invalid value must omit the header, not panic"
        );
    }
}
