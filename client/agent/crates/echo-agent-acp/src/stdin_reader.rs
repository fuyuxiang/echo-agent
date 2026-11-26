//! Dedicated-thread reader for the ACP stdio transport's standard input.
//!
//! Every ACP client (VS Code extension, echo-agent-desktop, the leader bridge) drives
//! the agent over a **persistent, bidirectional** newline-delimited JSON-RPC
//! stream on stdio: it writes requests on the child's stdin and reads responses
//! on stdout, keeping **stdin open for the whole session**.
//!
//! # Why not `tokio::io::stdin()`
//!
//! `tokio::io::stdin()` is not truly asynchronous. Tokio services it with a
//! blocking `std::io` read on an internal pool thread, and that read **cannot be
//! cancelled**. For interactive / persistent uses the
//! [`tokio::io::Stdin`](https://docs.rs/tokio/latest/tokio/io/struct.Stdin.html)
//! docs recommend "spawn a thread dedicated to user input and use blocking IO
//! directly in that thread". [`spawn_stdin_line_reader`] does exactly that.
//!
//! # Why the reader takes *exclusive* ownership of stdin (Windows)
//!
//! `std::io::Stdin` is a process-global handle guarded by a re-entrant mutex
//! (the `StdinLock`). A blocking read **holds that lock for the entire duration
//! of the read** — and for the persistent stdio transport the reader is almost
//! always parked in a read, waiting for the client's next line. If *any other*
//! code in the process then calls `std::io::stdin()` (e.g. a stray interactive
//! prompt reached only on a particular platform), it blocks on the lock until
//! the reader's in-flight read returns — which only happens at **EOF**, i.e.
//! when the client closes stdin. For a persistent ACP client that never closes
//! stdin mid-session this is a hard hang: the agent freezes part-way through a
//! request (observed on **Windows** during `session/new`) and only unblocks when
//! the transport is torn down. macOS/Linux don't reach the offending stray read,
//! so they were unaffected — but the hazard is real on any platform.
//!
//! To make the transport robust, on Windows the reader thread takes a **private
//! duplicate** of the real stdin handle and then points the process's standard
//! input at **`NUL`**. The reader keeps reading the client's bytes through its
//! private handle, while every *other* `std::io::stdin()` read in the process
//! observes immediate EOF instead of deadlocking on the lock. This mirrors what
//! already makes leader mode safe (the agent subprocess is spawned with
//! `stdin = NUL`, so its stray reads EOF instantly). Unix keeps reading
//! `std::io::stdin()` directly — it has no second stdin reader on these paths
//! and the extra FFI/`dup` would add risk for no benefit.
//!
//! # Escaped-slash normalization (acp 0.6 wire workaround)
//!
//! Every line is forwarded through `normalize_json_line` — see the
//! crate-private `normalize` module for the contract and its scope.

use std::io::BufRead;

use tokio::sync::mpsc;

use crate::normalize::normalize_json_line;

/// Channel depth for buffered stdin lines. Small: the reader thread blocks on a
/// full channel, applying natural backpressure to a flooding peer rather than
/// growing memory without bound.
const STDIN_LINE_CHANNEL_DEPTH: usize = 64;

/// Spawn a dedicated OS thread that reads newline-delimited lines from the
/// process's standard input with **synchronous, blocking** `std::io` and yields
/// each line (its trailing `\n` included, like `read_line`/`read_until`) on the
/// returned channel. A final line without a trailing newline is still delivered
/// before the channel closes.
///
/// Yielded lines are **not guaranteed byte-verbatim**: a line the pinned acp
/// 0.6 envelope would otherwise drop (a `\/`-escaped `method`, as Foundation
/// encoders emit) is re-serialized compactly (key order, whitespace, and
/// number formatting normalized) before forwarding — see the crate-private
/// `normalize` module. Every line the envelope already accepts, and anything
/// that fails to parse, passes through byte-identical (trailing terminator
/// always preserved).
///
/// The channel closes (so [`recv`](mpsc::Receiver::recv) returns `None`) when
/// stdin reaches EOF, the read fails, or the [`Receiver`](mpsc::Receiver) is
/// dropped. The reader is meant to be the **sole** stdin consumer in the
/// agent-stdio / leader-bridge paths; on Windows it enforces that by redirecting
/// the process's standard input to `NUL` so stray readers can't deadlock on it
/// (see the [module docs](self)).
pub fn spawn_stdin_line_reader() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(STDIN_LINE_CHANNEL_DEPTH);

    // On Windows, synchronously take a private duplicate of the real stdin and
    // redirect the process's standard input to `NUL` *before* the reader thread
    // parks in a blocking read holding the global `StdinLock`. After this, any
    // other `std::io::stdin()` read in the process EOFs immediately instead of
    // deadlocking. `None` means we couldn't isolate (we fall back to reading
    // `std::io::stdin()` directly — no worse than before).
    #[cfg(windows)]
    let private_stdin: Option<std::fs::File> = isolate_process_stdin();

    std::thread::Builder::new()
        .name("acp-stdin".to_string())
        .spawn(move || {
            #[cfg(windows)]
            if let Some(file) = private_stdin {
                forward_lines(std::io::BufReader::new(file), &tx);
                return;
            }
            let stdin = std::io::stdin();
            forward_lines(stdin.lock(), &tx);
        })
        .expect("failed to spawn acp-stdin reader thread");
    rx
}

/// Read `\n`-delimited lines from `reader` and forward each on `tx` — via
/// [`normalize_json_line`], so bytes are verbatim except for the lines that
/// workaround rewrites (terminator always preserved) — until EOF, a read
/// error, or the receiver is dropped.
fn forward_lines<R: BufRead>(mut reader: R, tx: &mpsc::Sender<Vec<u8>>) {
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            // EOF or a fatal read error: return, dropping `tx` closes the channel.
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let normalized = normalize_json_line(std::mem::take(&mut line));
        // `blocking_send` parks this thread (not a runtime worker) when the
        // channel is full, and errors only once the receiver is dropped — at
        // which point there is nothing left to feed.
        if tx.blocking_send(normalized).is_err() {
            break;
        }
    }
}

/// Duplicate the real stdin handle for private use and repoint the process's
/// `STD_INPUT_HANDLE` at `NUL`, returning the duplicate as an owned [`File`].
///
/// Returns `None` (caller falls back to `std::io::stdin()`) when there is no
/// stdin handle or duplication fails. Win32 declarations are inlined to avoid a
/// `windows`/`windows-sys` dependency, matching the pager's console setup.
///
/// [`File`]: std::fs::File
#[cfg(windows)]
fn isolate_process_stdin() -> Option<std::fs::File> {
    use std::os::windows::io::FromRawHandle as _;

    // Win32 constants (inlined to avoid a dependency).
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 0x0000_0003;
    const INVALID_HANDLE: *mut core::ffi::c_void = -1_isize as *mut core::ffi::c_void;

    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
        fn SetStdHandle(nStdHandle: u32, hHandle: *mut core::ffi::c_void) -> i32;
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn DuplicateHandle(
            hSourceProcessHandle: *mut core::ffi::c_void,
            hSourceHandle: *mut core::ffi::c_void,
            hTargetProcessHandle: *mut core::ffi::c_void,
            lpTargetHandle: *mut *mut core::ffi::c_void,
            dwDesiredAccess: u32,
            bInheritHandle: i32,
            dwOptions: u32,
        ) -> i32;
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut core::ffi::c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
    }

    // SAFETY: standard Win32 console/file calls; every return value is checked
    // before use and the duplicated handle is wrapped in an owning `File`.
    unsafe {
        let current = GetStdHandle(STD_INPUT_HANDLE);
        if current.is_null() || current == INVALID_HANDLE {
            return None;
        }

        let process = GetCurrentProcess();
        let mut duplicate: *mut core::ffi::c_void = std::ptr::null_mut();
        if DuplicateHandle(
            process,
            current,
            process,
            &mut duplicate,
            0,
            0, // not inheritable
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return None;
        }

        // Repoint the process's std input at NUL so stray `std::io::stdin()`
        // reads observe EOF instead of blocking on the held `StdinLock`. If NUL
        // can't be opened we still return the duplicate so the reader works;
        // we just forgo the stray-read isolation.
        let nul: Vec<u16> = "NUL\0".encode_utf16().collect();
        let nul_handle = CreateFileW(
            nul.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if nul_handle != INVALID_HANDLE && !nul_handle.is_null() {
            SetStdHandle(STD_INPUT_HANDLE, nul_handle);
        }

        Some(std::fs::File::from_raw_handle(duplicate as _))
    }
}
