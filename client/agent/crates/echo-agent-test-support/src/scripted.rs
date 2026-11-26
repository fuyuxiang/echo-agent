//! Data-driven scripted responses for the mock inference server: plain
//! status/header/body triples queued per path and rendered to HTTP at serve
//! time. Pure data — no router or handler types in the public surface.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;

use axum::Json;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use serde_json::Value;

pub(crate) type BoxWait = Pin<Box<dyn Future<Output = ()> + Send>>;
pub(crate) type TerminalWait = Box<dyn FnOnce() -> BoxWait + Send>;

/// One SSE event as data: optional `event:` name plus the `data:` payload.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

impl SseEvent {
    /// Event with a `data:` payload only.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
        }
    }

    /// Event with an `event:` name and a `data:` payload.
    pub fn with_event(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
        }
    }
}

/// Body of a [`ScriptedResponse`].
#[derive(Debug, Clone)]
pub enum ScriptedBody {
    Json(Value),
    Sse(Vec<SseEvent>),
    /// Raw body bytes, served verbatim (byte-controllable malformed SSE etc.).
    Raw(String),
}

/// A scripted reply served by a matched expectation or compatibility FIFO.
/// Scripted replies take precedence over required auth and fallback modes.
#[derive(Debug, Clone)]
pub struct ScriptedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ScriptedBody,
}

impl ScriptedResponse {
    /// 200 SSE response built from an event list.
    pub fn sse(events: Vec<SseEvent>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: ScriptedBody::Sse(events),
        }
    }

    /// JSON body with the given status.
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: ScriptedBody::Json(body),
        }
    }

    /// Raw text body with the given status.
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: ScriptedBody::Raw(body.into()),
        }
    }

    pub(crate) fn is_sse(&self) -> bool {
        matches!(self.body, ScriptedBody::Sse(_))
    }

    /// Validate status and headers eagerly so a bad script panics at the
    /// enqueue call site rather than far away at serve time.
    pub(crate) fn validate(&self) {
        StatusCode::from_u16(self.status).expect("invalid scripted status code");
        for (name, value) in &self.headers {
            HeaderName::from_bytes(name.as_bytes()).expect("invalid scripted header name");
            HeaderValue::from_str(value).expect("invalid scripted header value");
        }
    }

    /// Render to HTTP with SSE events paced by `delay` and optional terminal
    /// completion gating. Non-SSE bodies wait before returning so every body
    /// mode obeys the same release barrier.
    pub(crate) async fn into_response_paced(
        self,
        delay: Option<std::time::Duration>,
        before_terminal: Option<TerminalWait>,
    ) -> Response {
        let mut resp = match self.body {
            ScriptedBody::Json(v) => {
                if let Some(wait) = before_terminal {
                    wait().await;
                }
                Json(v).into_response()
            }
            ScriptedBody::Raw(s) => {
                if let Some(wait) = before_terminal {
                    wait().await;
                }
                s.into_response()
            }
            ScriptedBody::Sse(events) => {
                let last_idx = events.len().checked_sub(1);
                let mut events: Vec<_> = events.into_iter().enumerate().map(Some).collect();
                if events.is_empty() && before_terminal.is_some() {
                    events.push(None);
                }
                let stream = stream::unfold(
                    (events.into_iter(), before_terminal),
                    move |(mut events, mut before_terminal)| async move {
                        loop {
                            let item = events.next()?;
                            let Some((idx, scripted_event)) = item else {
                                if let Some(wait) = before_terminal.take() {
                                    wait().await;
                                }
                                continue;
                            };
                            if let Some(d) = delay {
                                tokio::time::sleep(d).await;
                            }
                            if Some(idx) == last_idx
                                && let Some(wait) = before_terminal.take()
                            {
                                wait().await;
                            }
                            let event =
                                axum::response::sse::Event::default().data(scripted_event.data);
                            let event = match scripted_event.event {
                                Some(name) => event.event(name),
                                None => event,
                            };
                            return Some((Ok::<_, Infallible>(event), (events, before_terminal)));
                        }
                    },
                );
                Sse::new(stream)
                    .keep_alive(KeepAlive::default())
                    .into_response()
            }
        };
        *resp.status_mut() = StatusCode::from_u16(self.status).expect("valid scripted status code");
        for (k, v) in self.headers {
            resp.headers_mut().insert(
                HeaderName::from_bytes(k.as_bytes()).expect("valid scripted header name"),
                HeaderValue::from_str(&v).expect("valid scripted header value"),
            );
        }
        resp
    }
}
