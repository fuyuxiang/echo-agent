//! Minimal connection-counting HTTP/1.1 server for wire-level tests that need
//! to assert TCP connection reuse (e.g. shared-client pooling): it counts
//! accepted connections and records each request's header block.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal keep-alive HTTP/1.1 server: counts accepted connections and
/// records each request's header block.
pub async fn spawn_counting_server() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let accepts = Arc::new(AtomicUsize::new(0));
    let heads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (accepts_l, heads_l) = (Arc::clone(&accepts), Arc::clone(&heads));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            accepts_l.fetch_add(1, Ordering::SeqCst);
            let heads = Arc::clone(&heads_l);
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    // Read one full request: header block, then content-length body bytes.
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        let mut chunk = [0u8; 4096];
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let body_len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + body_len {
                        let mut chunk = [0u8; 4096];
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    buf.drain(..head_end + body_len);
                    heads.lock().unwrap().push(head);
                    let resp =
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
                    if sock.write_all(resp).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (base_url, accepts, heads)
}
