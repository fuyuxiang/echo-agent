//! Explicit, single-task pairing with the browser extension over loopback WebSocket.
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
type Command = (Value, oneshot::Sender<Result<Value, String>>);
static CONNECTIONS: OnceLock<Mutex<HashMap<String, Arc<Connection>>>> = OnceLock::new();
static PAIRINGS: OnceLock<Mutex<HashMap<String, (String, CancellationToken)>>> = OnceLock::new();
fn pairings() -> &'static Mutex<HashMap<String, (String, CancellationToken)>> {
    PAIRINGS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn connections() -> &'static Mutex<HashMap<String, Arc<Connection>>> {
    CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
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
    connections()
        .lock()
        .unwrap()
        .get(session)
        .filter(|c| c.live())
        .cloned()
}
pub fn disconnect(session: &str) {
    if let Some((_, token)) = pairings().lock().unwrap().remove(session) {
        token.cancel();
    }
    if let Some(c) = connections().lock().unwrap().remove(session) {
        c.close();
    }
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
    disconnect(session);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let token = format!(
        "{}{}",
        uuid::Uuid::now_v7().simple(),
        uuid::Uuid::now_v7().simple()
    );
    let endpoint = format!("ws://{address}/{token}");
    let cancellation = CancellationToken::new();
    pairings()
        .lock()
        .unwrap()
        .insert(session.into(), (token.clone(), cancellation.clone()));
    let session = session.to_owned();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        loop {
            let accepted = tokio::select! {_=cancellation.cancelled()=>break,value=tokio::time::timeout_at(deadline,listener.accept())=>value};
            let Ok(Ok((socket, _))) = accepted else { break };
            let expected = format!("/{token}");
            let host = address.to_string();
            let handshake = tokio_tungstenite::accept_hdr_async(
                socket,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      response| {
                    let origin = request
                        .headers()
                        .get("origin")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if request.uri().path() != expected
                        || !origin.starts_with("chrome-extension://")
                        || request.headers().get("host").and_then(|v| v.to_str().ok())
                            != Some(host.as_str())
                    {
                        return Err(tokio_tungstenite::tungstenite::http::Response::builder()
                            .status(403)
                            .body(Some("pairing denied".into()))
                            .unwrap());
                    }
                    Ok(response)
                },
            );
            let Ok(Ok(socket)) = tokio::time::timeout(Duration::from_secs(3), handshake).await
            else {
                continue;
            };
            let (sender, mut commands) = mpsc::channel::<Command>(8);
            let connection = Arc::new(Connection {
                sender,
                live: AtomicBool::new(true),
                stop: CancellationToken::new(),
            });
            {
                let tickets = pairings().lock().unwrap();
                if cancellation.is_cancelled()
                    || tickets.get(&session).is_none_or(|(id, _)| id != &token)
                {
                    break;
                }
                connections()
                    .lock()
                    .unwrap()
                    .insert(session.clone(), connection.clone());
            }
            on_change(session.clone());
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
            connection.live.store(false, Ordering::SeqCst);
            on_change(session.clone());
            let _ = output.close().await;
            for (_, reply) in pending {
                let _ = reply.send(Err("browser_extension_disconnected: outcome unknown".into()));
            }
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
