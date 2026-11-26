//! Backtrace symbolication for crash reports.
//!
//! Runs at normal startup (not in a signal handler), so full Rust APIs
//! are available. Resolves raw instruction pointer addresses from the
//! crash blob into function names and file locations.

use crate::format::CrashBlob;

/// A resolved backtrace frame.
#[derive(Debug, Clone)]
pub struct ResolvedFrame {
    pub ip: usize,
    pub symbol_name: Option<String>,
    pub filename: Option<String>,
    pub lineno: Option<u32>,
}

/// Resolve raw instruction pointers from a crash blob into symbol names.
///
/// Uses the `backtrace` crate's `resolve` function. This works best when
/// the binary has debug info or at least a symbol table. For stripped
/// release binaries, symbol names may still be available (e.g.
/// `my_app::render::draw_frame`) but file/line info will
/// be missing.
pub fn resolve_frames(blob: &CrashBlob) -> Vec<ResolvedFrame> {
    blob.frames
        .iter()
        .map(|&ip| {
            let mut resolved = ResolvedFrame {
                ip,
                symbol_name: None,
                filename: None,
                lineno: None,
            };

            backtrace::resolve(ip as *mut std::ffi::c_void, |sym| {
                if resolved.symbol_name.is_none() {
                    resolved.symbol_name = sym.name().map(|n| n.to_string());
                    resolved.filename = sym.filename().map(|f| f.display().to_string());
                    resolved.lineno = sym.lineno();
                }
            });

            resolved
        })
        .collect()
}

/// Format a crash report as human-readable text.
pub fn format_report(blob: &CrashBlob, frames: &[ResolvedFrame]) -> String {
    let mut out = String::with_capacity(4096);

    out.push_str("=== EchoAgent Crash Report ===\n\n");

    out.push_str(&format!("Signal:  {}\n", signal_name(blob.signal)));
    out.push_str(&format!(
        "si_code: {} ({})\n",
        blob.si_code,
        si_code_name(blob.signal, blob.si_code)
    ));
    out.push_str(&format!("Address: {:#018x}\n", blob.si_addr));
    out.push_str(&format!("PID:     {}\n", blob.pid));
    out.push_str(&format!("Version: {}\n", blob.app_version));

    // Format timestamp as ISO 8601 (best-effort without chrono dependency).
    out.push_str(&format!("Time:    {} (unix)\n", blob.timestamp));

    out.push_str(&format!("\nBacktrace ({} frames):\n", frames.len()));
    for (i, frame) in frames.iter().enumerate() {
        let name = frame.symbol_name.as_deref().unwrap_or("<unknown>");
        out.push_str(&format!("  {:>3}: {:#018x} - {}\n", i, frame.ip, name));
        if let (Some(file), Some(line)) = (&frame.filename, frame.lineno) {
            out.push_str(&format!("           at {}:{}\n", file, line));
        }
    }

    out.push_str("\n=== End Report ===\n");
    out
}

pub fn signal_name(sig: u8) -> &'static str {
    match sig as i32 {
        4 => "SIGILL (Illegal instruction)",
        // SIGABRT is 6 on both macOS and Linux.
        // With panic = "abort", every Rust panic terminates via SIGABRT.
        6 => "SIGABRT (Abort)",
        // SIGBUS is 10 on macOS, 7 on Linux
        7 | 10 => "SIGBUS (Bus error)",
        11 => "SIGSEGV (Segmentation fault)",
        _ => "Unknown signal",
    }
}

fn si_code_name(sig: u8, code: i32) -> &'static str {
    // SIGABRT carries no fault-specific si_code (abort(3) raises it directly;
    // the kernel reports SI_USER/SI_TKILL-style origins instead).
    if sig == 6 {
        return "abort() - raised by the process (e.g. Rust panic with panic=abort)";
    }
    let is_bus = sig == 7 || sig == 10;
    if is_bus {
        match code {
            1 => "BUS_ADRALN - invalid address alignment",
            2 => "BUS_ADRERR - non-existent physical address",
            3 => "BUS_OBJERR - object-specific hardware error",
            _ => "unknown",
        }
    } else {
        match code {
            1 => "SEGV_MAPERR - address not mapped",
            2 => "SEGV_ACCERR - invalid permissions",
            _ => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_names() {
        assert_eq!(signal_name(6), "SIGABRT (Abort)");
        assert_eq!(signal_name(10), "SIGBUS (Bus error)");
        assert_eq!(signal_name(7), "SIGBUS (Bus error)");
        assert_eq!(signal_name(11), "SIGSEGV (Segmentation fault)");
    }

    #[test]
    fn format_report_smoke() {
        let blob = CrashBlob {
            signal: 10,
            si_code: 2,
            si_addr: 0x7f8a_1234_0000,
            pid: 42,
            timestamp: 1_712_678_587,
            frames: vec![0xdead_beef],
            app_version: "0.1.169".to_string(),
        };
        let frames = vec![ResolvedFrame {
            ip: 0xdead_beef,
            symbol_name: Some("echo_agent_pager::main".to_string()),
            filename: Some("src/main.rs".to_string()),
            lineno: Some(42),
        }];
        let report = format_report(&blob, &frames);
        assert!(report.contains("SIGBUS"));
        assert!(report.contains("BUS_ADRERR"));
        assert!(report.contains("echo_agent_pager::main"));
        assert!(report.contains("src/main.rs:42"));
    }
}
