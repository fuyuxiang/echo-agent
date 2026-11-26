# echo-agent-crash-handler

Crash handler for SIGBUS/SIGSEGV with best-effort backtrace capture.

## How it works

`install()` registers a `sigaction` handler. On crash it writes a binary blob (`GCRX` format) to `crash_dir/last-crash.bin` and restores the terminal via pre-computed escape sequences. The handler uses only async-signal-safe operations for file I/O, terminal restore, and re-raise.

On next launch, `check_previous_crash()` reads the blob, resolves IPs to symbols via `backtrace`, writes `last-crash-report.txt`, and archives it (keeping the last 5 reports).

No-ops on non-unix platforms. On musl-based Linux (release builds), the handler still records signal/address/version but skips frame capture since musl does not provide `backtrace()`.

## Limitations

### Frame capture is best-effort

Frame capture uses two fully async-signal-safe techniques:
1. The crash instruction pointer is extracted directly from the `ucontext_t` passed by the kernel.
2. Additional frames are captured by walking the frame-pointer chain (RBP on x86_64, x29 on aarch64) with raw pointer reads.

In release builds without `-C force-frame-pointers`, the frame-pointer chain may be incomplete or empty (the compiler omits frame pointers by default for optimization). The crash PC is always captured. In debug/dev builds, frame pointers are retained by default, producing fuller call stacks.

### sigaltstack is per-thread

The alternate signal stack is installed only on the thread that calls `install()`. Tokio worker threads do not inherit it. Stack overflows on worker threads will still trigger the handler (sigaction is process-wide), but without altstack protection the handler itself may fault on the overflowed stack.

## Usage

```rust
use std::path::PathBuf;

let crash_dir = PathBuf::from("/home/user/.myapp/crash");

// check_previous_crash MUST be called before install(), because
// install() opens last-crash.bin with O_TRUNC.
if let Some(r) = echo_agent_crash_handler::check_previous_crash(&crash_dir) {
    eprintln!("Crashed last session: {}", r.signal_name);
    eprintln!("Report: {}", r.report_path.display());
}

// install() before any threads or async runtime — sigaltstack is per-thread.
// Creates crash_dir if it does not exist.
echo_agent_crash_handler::install(echo_agent_crash_handler::CrashHandlerConfig {
    app_version: env!("CARGO_PKG_VERSION").to_string(),
    crash_dir,
});
```
