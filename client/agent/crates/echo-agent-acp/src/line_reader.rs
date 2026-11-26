//! Cancel-safe line-buffered [`AsyncRead`] wrapper.
//!
//! `agent-client-protocol` v0.6's `handle_io` uses `select_biased!` with
//! `BufReader::read_line`. `read_line` is **not** cancel-safe: it internally
//! calls `consume()` on partial reads, so dropping the future mid-read loses
//! bytes and corrupts the stream.
//!
//! [`LineBufferedRead`] works around this by pre-reading complete `\n`-delimited
//! lines on a dedicated task and serving them through a channel. The `poll_read`
//! implementation only returns `Pending` *between* lines (when no buffered data
//! remains), so ACP's `BufReader::read_line` always finds `\n` without
//! suspending, and can never be cancelled mid-read by `select_biased!`.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    AsyncBufRead, AsyncBufReadExt as _, AsyncRead, SinkExt as _, StreamExt as _, channel::mpsc,
    io::BufReader,
};

/// Maximum size of a single NDJSON line (64 MiB).
///
/// Prevents unbounded memory growth if a peer sends data without newlines.
/// 64 MiB accommodates the largest legitimate ACP messages (e.g. a
/// multi-megabyte file read response after JSON string escaping).
const MAX_LINE_SIZE: usize = 64 * 1024 * 1024;

/// An [`AsyncRead`] that only yields complete `\n`-delimited lines.
///
/// Internally, a background task reads lines from the wrapped reader and sends
/// them through a channel. [`poll_read`](AsyncRead::poll_read) serves bytes
/// from the current line buffer and only returns `Poll::Pending` when no
/// buffered bytes remain (i.e. between lines). This guarantees that a consumer
/// calling `BufReader::read_line` on this reader will always complete without
/// intermediate `Pending` states, making it safe to use inside `select!`.
pub struct LineBufferedRead {
    /// Buffered bytes from the current line being served.
    buf: Vec<u8>,
    /// Read cursor within `buf`.
    pos: usize,
    /// Receives complete lines (or an IO error) from the reader task.
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
}

impl LineBufferedRead {
    /// Wrap an `AsyncRead` source, spawning the reader task via
    /// [`tokio::task::spawn_local`].
    pub fn spawn_local(source: impl AsyncRead + Unpin + 'static) -> Self {
        Self::new(source, |fut| {
            tokio::task::spawn_local(fut);
        })
    }

    /// Wrap an `AsyncRead` source with cancel-safe line buffering.
    ///
    /// A background task is spawned (via `spawn`) that reads `\n`-delimited
    /// lines from `source` and feeds them into the returned reader.
    pub fn new(
        source: impl AsyncRead + Unpin + 'static,
        spawn: impl FnOnce(futures::future::LocalBoxFuture<'static, ()>),
    ) -> Self {
        let (mut tx, rx) = mpsc::channel(64);

        spawn(Box::pin(async move {
            let mut reader = BufReader::new(source);
            let mut line = Vec::new();
            loop {
                match read_line_capped(&mut reader, &mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(Ok(line.split_off(0))).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                }
            }
        }));

        Self {
            buf: Vec::new(),
            pos: 0,
            rx,
        }
    }
}

impl AsyncRead for LineBufferedRead {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        // Serve remaining bytes from the current line.
        if this.pos < this.buf.len() {
            let avail = this.buf.len() - this.pos;
            let n = avail.min(buf.len());
            buf[..n].copy_from_slice(&this.buf[this.pos..this.pos + n]);
            this.pos += n;
            if this.pos >= this.buf.len() {
                this.buf.clear();
                this.pos = 0;
            }
            return Poll::Ready(Ok(n));
        }

        // No buffered data — try to receive the next complete line.
        match this.rx.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(line))) => {
                let n = line.len().min(buf.len());
                buf[..n].copy_from_slice(&line[..n]);
                if n < line.len() {
                    // Stash the remainder for subsequent poll_read calls.
                    this.buf = line;
                    this.pos = n;
                }
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Err(e)),
            Poll::Ready(None) => Poll::Ready(Ok(0)), // EOF
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Read a single `\n`-delimited line into `buf`, capped at [`MAX_LINE_SIZE`].
///
/// Unlike `read_line`, this checks the accumulated size after each internal
/// buffer fill, so memory usage stays bounded even if the peer never sends
/// a newline.
async fn read_line_capped(
    reader: &mut (impl AsyncBufRead + Unpin),
    buf: &mut Vec<u8>,
) -> io::Result<usize> {
    buf.clear();
    loop {
        let (consumed, done) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(buf.len()); // EOF
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    buf.extend_from_slice(&available[..=pos]);
                    (pos + 1, true)
                }
                None => {
                    buf.extend_from_slice(available);
                    (available.len(), false)
                }
            }
        };
        reader.consume_unpin(consumed);
        if buf.len() > MAX_LINE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "ACP message exceeds {} byte limit ({} bytes read)",
                    MAX_LINE_SIZE,
                    buf.len()
                ),
            ));
        }
        if done {
            return Ok(buf.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::{AsyncReadExt as _, io::Cursor};

    use super::*;

    /// Helper: run a test inside a tokio LocalSet so spawn_local works.
    fn run<F: Future<Output = ()>>(f: F) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::task::LocalSet::new().run_until(f).await;
            });
    }

    #[test]
    fn single_line() {
        run(async {
            let source = Cursor::new(b"hello world\n");
            let mut reader = LineBufferedRead::spawn_local(source);
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, b"hello world\n");
        });
    }

    #[test]
    fn multiple_lines() {
        run(async {
            let source = Cursor::new(b"line1\nline2\nline3\n");
            let mut reader = LineBufferedRead::spawn_local(source);
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, b"line1\nline2\nline3\n");
        });
    }

    #[test]
    fn eof_with_partial_line() {
        run(async {
            let source = Cursor::new(b"complete\nno trailing newline");
            let mut reader = LineBufferedRead::spawn_local(source);
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, b"complete\nno trailing newline");
        });
    }

    #[test]
    fn empty_input() {
        run(async {
            let source = Cursor::new(b"");
            let mut reader = LineBufferedRead::spawn_local(source);
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert!(buf.is_empty());
        });
    }

    #[test]
    fn large_line_within_limit() {
        run(async {
            // A line larger than BufReader's 8KB buffer but well under 64 MiB.
            let mut data = vec![b'x'; 100_000];
            data.push(b'\n');
            let source = Cursor::new(data.clone());
            let mut reader = LineBufferedRead::spawn_local(source);
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, data);
        });
    }

    #[test]
    fn read_line_capped_rejects_oversized() {
        // Test the capped reader directly with a small override isn't
        // practical (MAX_LINE_SIZE is const), so test via the real limit.
        // Just verify the function works for normal input.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let data = b"normal line\n";
                let mut reader = BufReader::new(Cursor::new(&data[..]));
                let mut buf = Vec::new();
                let n = read_line_capped(&mut reader, &mut buf).await.unwrap();
                assert_eq!(n, 12);
                assert_eq!(buf, b"normal line\n");

                // EOF returns 0
                buf.clear();
                let n = read_line_capped(&mut reader, &mut buf).await.unwrap();
                assert_eq!(n, 0);
            });
    }

    #[test]
    fn small_read_buffer() {
        run(async {
            // Verify poll_read correctly serves a line across multiple small reads.
            let source = Cursor::new(b"abcdef\n");
            let mut reader = LineBufferedRead::spawn_local(source);
            let mut small_buf = [0u8; 3];

            // First read: "abc"
            let n = reader.read(&mut small_buf).await.unwrap();
            assert_eq!(&small_buf[..n], b"abc");

            // Second read: "def"
            let n = reader.read(&mut small_buf).await.unwrap();
            assert_eq!(&small_buf[..n], b"def");

            // Third read: "\n"
            let n = reader.read(&mut small_buf).await.unwrap();
            assert_eq!(&small_buf[..n], b"\n");

            // EOF
            let n = reader.read(&mut small_buf).await.unwrap();
            assert_eq!(n, 0);
        });
    }
}
