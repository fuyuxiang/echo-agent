//! Explicit, single-task pairing with the browser extension over loopback WebSocket.
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_util::sync::CancellationToken;

struct PairingHandshake {
    expected: String,
    host: String,
}

impl Callback for PairingHandshake {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if request.uri().path() != self.expected
            || !origin.starts_with("chrome-extension://")
            || request
                .headers()
                .get("host")
                .and_then(|value| value.to_str().ok())
                != Some(self.host.as_str())
        {
            return Err(tokio_tungstenite::tungstenite::http::Response::builder()
                .status(403)
                .body(Some("pairing denied".into()))
                .unwrap());
        }
        Ok(response)
    }
}

type Command = (Value, oneshot::Sender<Result<Value, String>>);
struct PairingSlot {
    token: String,
    cancellation: CancellationToken,
    connection: Option<Arc<Connection>>,
}
impl Drop for PairingSlot {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(connection) = &self.connection {
            connection.close();
        }
    }
}
static PAIRINGS: OnceLock<Mutex<HashMap<String, PairingSlot>>> = OnceLock::new();
fn pairings() -> &'static Mutex<HashMap<String, PairingSlot>> {
    PAIRINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Own the registry entry as long as binding or serving the ticket is in flight.
struct PairingReservation {
    session: String,
    token: String,
    cancellation: CancellationToken,
}
impl PairingReservation {
    fn new(session: &str) -> Self {
        let token = format!(
            "{}{}",
            uuid::Uuid::now_v7().simple(),
            uuid::Uuid::now_v7().simple()
        );
        let cancellation = CancellationToken::new();
        pairings().lock().unwrap().insert(
            session.into(),
            PairingSlot {
                token: token.clone(),
                cancellation: cancellation.clone(),
                connection: None,
            },
        );
        Self {
            session: session.into(),
            token,
            cancellation,
        }
    }

    fn connect(&self, connection: Arc<Connection>) -> bool {
        let mut slots = pairings().lock().unwrap();
        let Some(slot) = slots.get_mut(&self.session) else {
            return false;
        };
        if slot.token != self.token {
            return false;
        }
        slot.connection = Some(connection);
        true
    }

    fn remove(&self) -> bool {
        let mut slots = pairings().lock().unwrap();
        if slots
            .get(&self.session)
            .is_some_and(|slot| slot.token == self.token)
        {
            slots.remove(&self.session);
            return true;
        }
        false
    }
}
impl Drop for PairingReservation {
    fn drop(&mut self) {
        self.remove();
    }
}

pub struct Connection {
    sender: mpsc::Sender<Command>,
    live: AtomicBool,
    stop: CancellationToken,
}
impl Connection {
    pub async fn pause(&self) -> Result<(), String> {
        self.call("Echo.pause", json!({}), None).await.map(|_| ())
    }
    pub async fn resume(&self) -> Result<(), String> {
        self.call("Echo.resume", json!({}), None).await.map(|_| ())
    }
    pub fn live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
    pub fn close(&self) {
        self.live.store(false, Ordering::SeqCst);
        self.stop.cancel();
    }
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        target: Option<&str>,
    ) -> Result<Value, String> {
        if !self.live() {
            return Err("browser_extension_disconnected".into());
        }
        let (tx, rx) = oneshot::channel();
        self.sender.send((json!({"id":uuid::Uuid::now_v7().to_string(),"method":method,"params":params,"targetId":target}),tx)).await.map_err(|_|"browser_extension_disconnected")?;
        tokio::time::timeout(Duration::from_secs(25), rx)
            .await
            .map_err(|_| "browser_extension_timeout: outcome unknown; observe before retry")?
            .map_err(|_| "browser_extension_disconnected")?
    }
}
pub fn connected(session: &str) -> Option<Arc<Connection>> {
    pairings()
        .lock()
        .unwrap()
        .get(session)
        .and_then(|slot| slot.connection.as_ref())
        .filter(|connection| connection.live())
        .cloned()
}
pub fn disconnect(session: &str) {
    pairings().lock().unwrap().remove(session);
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    pub endpoint: String,
    pub expires_in_seconds: u64,
    pub extension_path: String,
}
pub async fn pair(
    session: &str,
    extension_path: String,
    on_change: fn(String),
) -> Result<Pairing, String> {
    pair_with_listener(
        session,
        extension_path,
        on_change,
        tokio::net::TcpListener::bind("127.0.0.1:0"),
    )
    .await
}

async fn pair_with_listener(
    session: &str,
    extension_path: String,
    on_change: fn(String),
    bind: impl Future<Output = std::io::Result<tokio::net::TcpListener>>,
) -> Result<Pairing, String> {
    let reservation = PairingReservation::new(session);
    let listener = tokio::select! {
        biased;
        _ = reservation.cancellation.cancelled() => return Err("browser_extension_pairing_cancelled".into()),
        result = bind => result.map_err(|error| error.to_string())?,
    };
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let endpoint = format!("ws://{address}/{}", reservation.token);
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        loop {
            let accepted = tokio::select! {
                biased;
                _ = reservation.cancellation.cancelled() => break,
                value = tokio::time::timeout_at(deadline, listener.accept()) => value,
            };
            let Ok(Ok((socket, _))) = accepted else { break };
            let expected = format!("/{}", reservation.token);
            let host = address.to_string();
            let handshake =
                tokio_tungstenite::accept_hdr_async(socket, PairingHandshake { expected, host });
            let handshake_deadline =
                deadline.min(tokio::time::Instant::now() + Duration::from_secs(3));
            let result = tokio::select! {
                biased;
                _ = reservation.cancellation.cancelled() => break,
                value = tokio::time::timeout_at(handshake_deadline, handshake) => value,
            };
            let Ok(Ok(socket)) = result else { continue };
            let (sender, mut commands) = mpsc::channel::<Command>(8);
            let connection = Arc::new(Connection {
                sender,
                live: AtomicBool::new(true),
                stop: CancellationToken::new(),
            });
            if !reservation.connect(connection.clone()) {
                break;
            }
            on_change(reservation.session.clone());
            let (mut output, mut input) = socket.split();
            let mut pending: HashMap<String, oneshot::Sender<Result<Value, String>>> =
                HashMap::new();
            loop {
                tokio::select! {
                    _=connection.stop.cancelled()=>break,
                    command=commands.recv()=>match command {
                        Some((command,reply))=>{let id=command["id"].as_str().unwrap().to_owned();pending.retain(|_,reply|!reply.is_closed());if pending.len()>=32{let _=reply.send(Err("browser_extension_busy".into()));continue;}pending.insert(id,reply);if output.send(tokio_tungstenite::tungstenite::Message::Text(command.to_string().into())).await.is_err(){break}},
                        None=>break,
                    },
                    message=input.next()=>match message {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) if text.len()<=24*1024*1024=>{
                            if let Ok(value)=serde_json::from_str::<Value>(&text){if let Some(reply)=value["id"].as_str().and_then(|id|pending.remove(id)){let result=if let Some(error)=value["error"].as_str(){Err(error.into())}else{Ok(value["result"].clone())};let _=reply.send(result);}}
                        },
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(bytes)))=>{let _=output.send(tokio_tungstenite::tungstenite::Message::Pong(bytes)).await;},
                        _=>break,
                    }
                }
            }
            connection.close();
            if reservation.remove() {
                on_change(reservation.session.clone());
            }
            for (_, reply) in pending {
                let _ = reply.send(Err("browser_extension_disconnected: outcome unknown".into()));
            }
            let _ = tokio::time::timeout(Duration::from_secs(3), output.close()).await;
            break;
        }
    });
    Ok(Pairing {
        endpoint,
        expires_in_seconds: 300,
        extension_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    #[test]
    fn pairing_checks_token_host_and_extension_origin_independently() {
        for (path, host, origin, accepted) in [
            (
                "/token",
                "127.0.0.1:1234",
                "chrome-extension://synthetic",
                true,
            ),
            (
                "/wrong",
                "127.0.0.1:1234",
                "chrome-extension://synthetic",
                false,
            ),
            (
                "/token",
                "untrusted.example",
                "chrome-extension://synthetic",
                false,
            ),
            (
                "/token",
                "127.0.0.1:1234",
                "https://untrusted.example",
                false,
            ),
        ] {
            let request = Request::builder()
                .uri(path)
                .header("host", host)
                .header("origin", origin)
                .body(())
                .unwrap();
            let response = Response::builder().status(101).body(()).unwrap();
            let result = PairingHandshake {
                expected: "/token".into(),
                host: "127.0.0.1:1234".into(),
            }
            .on_request(&request, response);
            if accepted {
                assert_eq!(result.unwrap().status(), 101);
            } else {
                assert_eq!(result.unwrap_err().status(), 403);
            }
        }
    }
    #[tokio::test]
    async fn pairing_requires_extension_origin_and_routes_only_its_session() {
        let session = uuid::Uuid::now_v7().to_string();
        let pairing = pair(&session, "/synthetic-extension".into(), |_| {})
            .await
            .unwrap();
        let mut bad = pairing.endpoint.clone().into_client_request().unwrap();
        bad.headers_mut()
            .insert("origin", "https://untrusted.example".parse().unwrap());
        assert!(tokio_tungstenite::connect_async(bad).await.is_err());
        let mut request = pairing.endpoint.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "chrome-extension://synthetic".parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        for _ in 0..100 {
            if connected(&session).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(connected("other-task").is_none());
        let connection = connected(&session).unwrap();
        let client = tokio::spawn(async move {
            let message = socket.next().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(value["method"], "Echo.tabs");
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"id":value["id"],"result":[{"id":"42"}]})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let _ = socket.next().await;
        });
        assert_eq!(
            connection.call("Echo.tabs", json!({}), None).await.unwrap()[0]["id"],
            "42"
        );
        disconnect(&session);
        client.await.unwrap();
        assert!(connected(&session).is_none());
    }
    #[tokio::test]
    async fn disconnect_while_listener_binds_revokes_pending_pairing() {
        let session = uuid::Uuid::now_v7().to_string();
        let (binding, started) = oneshot::channel();
        let (release, proceed) = oneshot::channel();
        let pending_session = session.clone();
        let pending = tokio::spawn(async move {
            pair_with_listener(&pending_session, "/fixture".into(), |_| {}, async {
                binding.send(()).unwrap();
                proceed.await.unwrap();
                tokio::net::TcpListener::bind("127.0.0.1:0").await
            })
            .await
        });
        started.await.unwrap();
        disconnect(&session);
        let _ = release.send(());
        assert!(pending.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn newer_pairing_survives_a_slow_older_listener() {
        let session = uuid::Uuid::now_v7().to_string();
        let (binding, started) = oneshot::channel();
        let (release, proceed) = oneshot::channel();
        let pending_session = session.clone();
        let pending = tokio::spawn(async move {
            pair_with_listener(&pending_session, "/fixture".into(), |_| {}, async {
                binding.send(()).unwrap();
                proceed.await.unwrap();
                tokio::net::TcpListener::bind("127.0.0.1:0").await
            })
            .await
        });
        started.await.unwrap();
        let current = pair(&session, "/fixture".into(), |_| {}).await.unwrap();
        let _ = release.send(());
        assert!(pending.await.unwrap().is_err());
        let mut request = current.endpoint.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "chrome-extension://synthetic".parse().unwrap());
        let (_socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while connected(&session).is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        disconnect(&session);
    }

    #[tokio::test]
    async fn replacing_pairing_revokes_the_old_endpoint() {
        let session = uuid::Uuid::now_v7().to_string();
        let old = pair(&session, "/fixture".into(), |_| {}).await.unwrap();
        let current = pair(&session, "/fixture".into(), |_| {}).await.unwrap();
        let mut request = old.endpoint.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "chrome-extension://synthetic".parse().unwrap());
        assert!(tokio_tungstenite::connect_async(request).await.is_err());
        assert!(current.endpoint.starts_with("ws://127.0.0.1:"));
        disconnect(&session);
    }
}
