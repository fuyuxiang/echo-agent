//! Controllable raw HTTP/1.1 artifact server shared by the blitz
//! download/install tests and the concurrent-update convergence tests.
//!
//! Serves a real executable artifact and can truncate the body, close the
//! connection early, serve a right-length-but-garbage body, or hang
//! mid-transfer — for both the parallel byte-range path and the
//! single-connection path. It also counts body-serving GETs (HEAD probes are
//! excluded) so tests can assert how many downloads actually happened, and
//! supports a "slow" mode that widens the race window so concurrent
//! installers genuinely overlap in flight.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How the server corrupts (or doesn't) the next download.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    /// Serve the real artifact correctly.
    Full,
    /// Serve a right-length body that exits non-zero (fails the smoke-test).
    Garbage,
    /// Advertise the full length but send only `k` bytes then close the socket
    /// (silent truncation: premature EOF / short range chunk).
    Truncate(usize),
    /// Send `k` bytes then hang, so a client-side timeout cancels mid-transfer.
    Hang(usize),
}

struct ServerState {
    body: Arc<Vec<u8>>,
    mode: Mode,
}

pub struct ArtifactServer {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<ServerState>>,
    shutdown: Arc<AtomicBool>,
    gets: Arc<AtomicUsize>,
    slow: Arc<AtomicBool>,
}

impl ArtifactServer {
    pub fn start(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ServerState {
            body: Arc::new(body),
            mode: Mode::Full,
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let gets = Arc::new(AtomicUsize::new(0));
        let slow = Arc::new(AtomicBool::new(false));

        let st = state.clone();
        let sd = shutdown.clone();
        let gc = gets.clone();
        let sl = slow.clone();
        std::thread::spawn(move || {
            while !sd.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let st = st.clone();
                        let sd = sd.clone();
                        let gc = gc.clone();
                        let sl = sl.clone();
                        std::thread::spawn(move || handle_connection(stream, st, sd, gc, sl));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            state,
            shutdown,
            gets,
            slow,
        }
    }

    pub fn uri(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn set_mode(&self, mode: Mode) {
        self.state.lock().unwrap().mode = mode;
    }

    /// Number of body-serving GET requests handled so far (HEAD probes from
    /// the parallel-download path are excluded). Tests use this to assert
    /// how many downloads actually happened — e.g. that a sequential updater
    /// converged onto an already-installed binary without re-downloading.
    /// One download may span multiple GETs when the parallel byte-range path
    /// splits it, so tests asserting exact counts use a small artifact
    /// (single-connection path, 1 GET per download).
    pub fn request_count(&self) -> usize {
        self.gets.load(Ordering::Relaxed)
    }

    /// When enabled, hold each Full/Garbage response open ~500ms before
    /// sending the body. This keeps an installer in flight long enough for
    /// concurrent installers to genuinely overlap even on a heavily loaded
    /// CI host — a too-short hold would let race tests run the installers
    /// back-to-back and never exercise the concurrent window.
    pub fn set_slow(&self, slow: bool) {
        self.slow.store(slow, Ordering::Relaxed);
    }
}

impl Drop for ArtifactServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Parse `Range: bytes=a-b` from a raw request header block (case-insensitive).
fn parse_range(request: &str) -> Option<(usize, usize)> {
    for line in request.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("range:") {
            let spec = rest.trim().strip_prefix("bytes=")?;
            let (a, b) = spec.split_once('-')?;
            return Some((a.trim().parse().ok()?, b.trim().parse().ok()?));
        }
    }
    None
}

fn handle_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<ServerState>>,
    shutdown: Arc<AtomicBool>,
    gets: Arc<AtomicUsize>,
    slow: Arc<AtomicBool>,
) {
    // A stream accepted from a non-blocking listener can inherit non-blocking
    // mode; force blocking so large `write_all`s don't short-write on WouldBlock.
    let _ = stream.set_nonblocking(false);
    // Avoid Nagle/delayed-ACK stalls on the header-then-body writes.
    let _ = stream.set_nodelay(true);

    // Read the request header block (until CRLFCRLF). Bodies are never sent by
    // the client, so headers are all we need.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&buf).to_string();
    let is_head = request.starts_with("HEAD");
    // Count only body-serving GETs; the parallel path's HEAD probe is excluded.
    if !is_head {
        gets.fetch_add(1, Ordering::Relaxed);
    }
    let range = parse_range(&request);

    let (body, mode) = {
        let st = state.lock().unwrap();
        (st.body.clone(), st.mode)
    };
    let total = body.len();
    let body: &[u8] = &body;

    // Determine the byte slice this request is for, plus the length we will
    // claim in Content-Length.
    let (slice_start, slice_end_excl) = match range {
        Some((a, b)) => (a.min(total), (b + 1).min(total)),
        None => (0, total),
    };
    let claimed_len = slice_end_excl - slice_start;

    // For truncation/hang, `k` is a GLOBAL cutoff across the whole artifact:
    // a slice that reaches past byte `k` is sent short, so the parallel path's
    // later chunk (or the single-connection body) is the one truncated.
    let send_end = match mode {
        Mode::Truncate(k) | Mode::Hang(k) => slice_end_excl.min(k).max(slice_start),
        _ => slice_end_excl,
    };
    // `payload` is what we actually transmit before any early close; for the
    // truncated modes it may be shorter than the advertised `claimed_len`.
    let payload: Vec<u8> = match mode {
        Mode::Garbage => {
            let mut bad = b"#!/bin/sh\nexit 1\n".to_vec();
            bad.resize(claimed_len, b'\n');
            bad
        }
        _ => body[slice_start..send_end].to_vec(),
    };

    // Status line + headers. For range requests we answer 206; HEAD is 200.
    let mut head = String::new();
    if range.is_some() && !is_head {
        head.push_str("HTTP/1.1 206 Partial Content\r\n");
        head.push_str(&format!(
            "Content-Range: bytes {}-{}/{}\r\n",
            slice_start,
            slice_end_excl.saturating_sub(1),
            total
        ));
    } else {
        head.push_str("HTTP/1.1 200 OK\r\n");
        head.push_str("Accept-Ranges: bytes\r\n");
    }
    // Always advertise the (claimed) full length so a truncated transfer is a
    // genuine premature EOF rather than a short-but-consistent body.
    head.push_str(&format!("Content-Length: {}\r\n", claimed_len));
    head.push_str("Connection: close\r\n\r\n");

    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    if is_head {
        let _ = stream.flush();
        return;
    }

    match mode {
        Mode::Full | Mode::Garbage => {
            // Hold the connection open longer so concurrent installers
            // genuinely overlap mid-download (see `set_slow`).
            if slow.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
            }
            let _ = stream.write_all(&payload);
        }
        Mode::Truncate(_) => {
            // Send the (possibly short) payload then drop the connection without
            // meeting Content-Length — the client sees a premature EOF.
            let _ = stream.write_all(&payload);
        }
        Mode::Hang(_) => {
            let _ = stream.write_all(&payload);
            let _ = stream.flush();
            // Hold the connection open longer than any client-side cancel
            // timeout so the client times out and cancels (a genuine mid-flight
            // cancel rather than a server-side close).
            for _ in 0..30 {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let _ = stream.flush();
}
