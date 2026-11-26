//! Cross-platform IPC transport for leader<->client communication.
//!
//! - **Unix:** [`LeaderStream`] / [`LeaderListener`] are type aliases for
//!   `tokio::net::UnixStream` / `UnixListener`. Zero wrapper, no unsafe.
//! - **Windows:** wraps `tokio::net::windows::named_pipe::*` (tokio doesn't
//!   expose AF_UNIX on Windows). The leader's filesystem path is hashed
//!   into `\\.\pipe\echo-agent-leader-<hash>` so callers keep their path-based API.
//!
#[cfg(unix)]
pub(super) use tokio::net::UnixListener as LeaderListener;
#[cfg(unix)]
pub(super) use tokio::net::UnixStream as LeaderStream;

/// Has a leader bound a listener at `path`?
///
/// - Unix: stats the socket file.
/// - Windows: probes the named pipe (Named Pipes don't appear in the
///   filesystem, so `path.exists()` doesn't work).
pub fn listener_is_ready(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        path.exists()
    }
    #[cfg(windows)]
    {
        windows_impl::listener_is_ready(path)
    }
}

#[cfg(windows)]
pub(super) use windows_impl::{LeaderListener, LeaderStream};

#[cfg(windows)]
mod windows_impl {
    use std::io;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tracing::debug;

    /// Bidirectional IPC stream wrapping a connected named pipe (server-
    /// or client-side, depending on how it was created).
    pub(crate) struct LeaderStream {
        inner: StreamInner,
    }

    enum StreamInner {
        Server(tokio::net::windows::named_pipe::NamedPipeServer),
        Client(tokio::net::windows::named_pipe::NamedPipeClient),
    }

    impl LeaderStream {
        /// Connect to a listener at `path`. The path is translated to a
        /// named-pipe name and `ClientOptions::open` is used.
        pub(crate) async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            use tokio::net::windows::named_pipe::ClientOptions;

            // ClientOptions::open returns ERROR_PIPE_BUSY if all pipe
            // instances are in use; the caller's CONNECT_TIMEOUT loop
            // already retries, so we surface the error and let it handle.
            let pipe_name = path_to_pipe_name(path.as_ref());
            let inner = ClientOptions::new().open(pipe_name)?;
            Ok(Self {
                inner: StreamInner::Client(inner),
            })
        }
    }

    // tokio's NamedPipeServer / NamedPipeClient are auto-Unpin (they wrap
    // PollEvented<mio::windows::NamedPipe>, which is Unpin), so our
    // wrapping enum and struct are auto-Unpin as well. That means
    // Pin<&mut Self>::get_mut() is safe — no unsafe needed for the
    // structural projection into `inner`.
    impl AsyncRead for LeaderStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match &mut self.get_mut().inner {
                StreamInner::Server(s) => Pin::new(s).poll_read(cx, buf),
                StreamInner::Client(c) => Pin::new(c).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for LeaderStream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match &mut self.get_mut().inner {
                StreamInner::Server(s) => Pin::new(s).poll_write(cx, buf),
                StreamInner::Client(c) => Pin::new(c).poll_write(cx, buf),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match &mut self.get_mut().inner {
                StreamInner::Server(s) => Pin::new(s).poll_flush(cx),
                StreamInner::Client(c) => Pin::new(c).poll_flush(cx),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match &mut self.get_mut().inner {
                StreamInner::Server(s) => Pin::new(s).poll_shutdown(cx),
                StreamInner::Client(c) => Pin::new(c).poll_shutdown(cx),
            }
        }
    }

    /// Listener for incoming leader IPC connections. Holds the pipe name
    /// plus the next pre-created server instance (Windows named pipes
    /// require pre-creating an instance per pending connection).
    pub(crate) struct LeaderListener {
        pipe_name: std::ffi::OsString,
        /// Next pre-created server instance, ready for `connect().await`.
        /// We rotate: take this one, await its connect, immediately create
        /// the next one for the following accept(). The first instance is
        /// created in `bind()` with `first_pipe_instance(true)` to lock
        /// out other processes from squatting the pipe name.
        ///
        /// tokio::sync::Mutex (not parking_lot) because accept() holds the
        /// lock across `server.connect().await`.
        next_server: tokio::sync::Mutex<Option<tokio::net::windows::named_pipe::NamedPipeServer>>,
    }

    impl LeaderListener {
        /// Reserve a named-pipe name (no on-disk file is created).
        pub(crate) fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            use tokio::net::windows::named_pipe::ServerOptions;

            let pipe_name = path_to_pipe_name(path.as_ref());
            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)?;
            Ok(Self {
                pipe_name,
                next_server: tokio::sync::Mutex::new(Some(first)),
            })
        }

        /// Wait for the next incoming connection. Mirrors
        /// `UnixListener::accept`, returning a connected stream and a unit
        /// placeholder where Unix would return the peer address (named
        /// pipes don't carry one).
        pub(crate) async fn accept(&self) -> io::Result<(LeaderStream, ())> {
            use tokio::net::windows::named_pipe::ServerOptions;

            // Take the pending instance (or create one), await a client, then
            // pre-create the next. On connect() error, drop the instance and
            // retry with a fresh one — returning early would leave the slot
            // empty and brick the listener. Bounded with a backoff so a
            // persistently failing connect() can't busy-spin.
            const MAX_ACCEPT_ATTEMPTS: usize = 10;
            const RETRY_BACKOFF: Duration = Duration::from_millis(20);

            let mut slot = self.next_server.lock().await;
            let mut last_err: Option<io::Error> = None;
            for attempt in 0..MAX_ACCEPT_ATTEMPTS {
                let server = match slot.take() {
                    Some(server) => server,
                    None => ServerOptions::new().create(&self.pipe_name)?,
                };
                match server.connect().await {
                    Ok(()) => {
                        *slot = Some(ServerOptions::new().create(&self.pipe_name)?);
                        return Ok((
                            LeaderStream {
                                inner: StreamInner::Server(server),
                            },
                            (),
                        ));
                    }
                    Err(e) => {
                        // Failed `server` drops here, freeing the instance.
                        debug!(attempt, error = %e, "named-pipe accept connect failed; retrying");
                        last_err = Some(e);
                        tokio::time::sleep(RETRY_BACKOFF).await;
                    }
                }
            }

            // Best-effort re-arm; take-or-create above still recovers if this fails.
            if let Ok(fresh) = ServerOptions::new().create(&self.pipe_name) {
                *slot = Some(fresh);
            }
            Err(last_err
                .unwrap_or_else(|| io::Error::other("LeaderListener: accept exhausted retries")))
        }
    }

    /// Whether a leader has a pipe bound at `path`.
    ///
    /// Probes with `WaitNamedPipeW` (non-connecting), not `ClientOptions::open`,
    /// which would open a real client the leader's `accept()` consumes as a
    /// phantom session. `ERROR_FILE_NOT_FOUND` means absent; `TRUE` or any other
    /// error (e.g. `ERROR_SEM_TIMEOUT`: exists but busy) means ready.
    pub(super) fn listener_is_ready(path: &Path) -> bool {
        use std::os::windows::ffi::OsStrExt;

        use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, GetLastError};
        use windows::Win32::System::Pipes::WaitNamedPipeW;
        use windows::core::PCWSTR;

        // 1 ms (a real timeout, not 0 = "server default").
        const PROBE_TIMEOUT_MS: u32 = 1;

        let pipe_name = path_to_pipe_name(path);
        let wide: Vec<u16> = pipe_name
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        if unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), PROBE_TIMEOUT_MS) }.as_bool() {
            return true;
        }
        // FALSE: only a missing pipe means not-ready.
        let err = unsafe { GetLastError() };
        err != ERROR_FILE_NOT_FOUND
    }

    /// Full named-pipe path: `\\.\pipe\<leaf>`.
    fn path_to_pipe_name(path: &Path) -> std::ffi::OsString {
        let mut name = std::ffi::OsString::from(r"\\.\pipe\");
        name.push(pipe_leaf_name(path));
        name
    }

    /// Deterministic leaf name (`echo-agent-leader-<hash>`) for a filesystem path.
    ///
    /// Uses SipHash-1-3 with fixed keys so the hash is stable across Rust
    /// versions (unlike `DefaultHasher`, whose algorithm is unspecified).
    fn pipe_leaf_name(path: &Path) -> std::ffi::OsString {
        use siphasher::sip::SipHasher13;
        use std::hash::{Hash, Hasher};

        // Fixed keys — must never change once shipped.
        let mut hasher = SipHasher13::new_with_keys(0x67726f6b_6c656164, 0x65725f70_69706521);
        path.hash(&mut hasher);
        let hash = hasher.finish();
        std::ffi::OsString::from(format!("echo-agent-leader-{hash:016x}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::Path;

        #[test]
        fn pipe_name_is_deterministic() {
            let a = path_to_pipe_name(Path::new("/tmp/echoagent.sock"));
            let b = path_to_pipe_name(Path::new("/tmp/echoagent.sock"));
            assert_eq!(a, b);
        }

        #[test]
        fn different_paths_produce_different_names() {
            let a = path_to_pipe_name(Path::new("/tmp/a.sock"));
            let b = path_to_pipe_name(Path::new("/tmp/b.sock"));
            assert_ne!(a, b);
        }

        #[test]
        fn pipe_name_has_correct_prefix() {
            let name = path_to_pipe_name(Path::new("/tmp/test.sock"));
            let s = name.to_string_lossy();
            assert!(s.starts_with(r"\\.\pipe\echo-agent-leader-"), "got: {s}");
        }

        #[test]
        fn pipe_name_is_bounded() {
            let long_path = "/".to_owned() + &"a".repeat(500);
            let name = path_to_pipe_name(Path::new(&long_path));
            // \\.\pipe\echo-agent-leader- (20 chars) + 16 hex chars = 36 total
            assert!(name.len() <= 256, "pipe name too long: {}", name.len());
        }

        #[tokio::test]
        async fn listener_is_ready_tracks_pipe_lifecycle() {
            // Unique path per process so parallel test binaries don't collide on
            // the derived pipe name.
            let path = std::env::temp_dir().join(format!(
                "echo-agent-ready-probe-{}.sock",
                std::process::id()
            ));

            // Nothing bound yet -> ERROR_FILE_NOT_FOUND -> not ready.
            assert!(!listener_is_ready(&path));

            let listener = LeaderListener::bind(&path).unwrap();
            // Ready as soon as the pipe is bound, before any accept().
            assert!(listener_is_ready(&path));

            // After the last instance is dropped the pipe name disappears.
            drop(listener);
            assert!(!listener_is_ready(&path));
        }
    }
}
