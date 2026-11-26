//! Cross-platform crash handler for fatal memory faults and aborts.
//!
//! - **Unix**: SIGBUS/SIGSEGV/SIGABRT via `sigaction(2)`. SIGABRT matters
//!   because release builds ship with `panic = "abort"`, so every Rust panic
//!   terminates via `abort(3)` — without a SIGABRT handler those deaths leave
//!   no crash report.
//! - **Windows**: `EXCEPTION_ACCESS_VIOLATION` et al. via `SetUnhandledExceptionFilter`.
//!   `abort()` does not go through the unhandled-exception filter, so SIGABRT
//!   capture is Unix-only.
//!
//! Captures crash PC + frame-pointer chain. All handler operations are
//! minimal (raw pointer reads, direct file I/O, atomics — no allocation).
//! The crash PC is written to disk before frame walking so a secondary
//! fault during the walk still produces a usable report.

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use crate::format::{self, MAX_FILE_SIZE, MAX_FRAMES};
    use crate::terminal;

    // ── Platform-specific ucontext access ────────────────────────────────
    //
    // The libc crate does not expose ucontext_t on macOS. We define minimal
    // repr(C) types covering only the fields we need (PC and frame pointer).

    /// Extract the crash instruction pointer and frame pointer from the
    /// signal handler's context parameter.
    ///
    /// Returns `(instruction_pointer, frame_pointer)`. Both may be 0 if
    /// the context is null or the platform is unsupported.
    unsafe fn extract_pc_and_fp(ctx: *mut libc::c_void) -> (usize, usize) {
        if ctx.is_null() {
            return (0, 0);
        }

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        unsafe {
            let uc = ctx as *const libc::ucontext_t;
            let gregs = &(*uc).uc_mcontext.gregs;
            let ip = gregs[libc::REG_RIP as usize] as usize;
            let fp = gregs[libc::REG_RBP as usize] as usize;
            return (ip, fp);
        }

        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        unsafe {
            let uc = ctx as *const libc::ucontext_t;
            let mc = &(*uc).uc_mcontext;
            let ip = mc.pc as usize;
            let fp = mc.regs[29] as usize; // x29 = frame pointer
            return (ip, fp);
        }

        // macOS does not expose ucontext_t in the libc crate.
        // Define minimal repr(C) types for the fields we need.
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            #[repr(C)]
            struct Arm64ThreadState {
                regs: [u64; 29], // x0-x28
                fp: u64,         // x29
                lr: u64,         // x30
                sp: u64,
                pc: u64,
                cpsr: u32,
                _pad: u32,
            }
            #[repr(C)]
            struct MachMcontext {
                _es: [u8; 16], // __darwin_arm_exception_state64 (far:u64 + esr:u32 + exception:u32)
                ss: Arm64ThreadState,
                // neon state follows but we don't need it
            }
            #[repr(C)]
            struct DarwinUcontext {
                _onstack: i32,
                _sigmask: u32,
                _stack: libc::stack_t,
                _link: *mut libc::c_void,
                _mcsize: usize,
                mctx: *const MachMcontext,
            }
            let (ip, fp) = unsafe {
                let uc = ctx as *const DarwinUcontext;
                let mctx = (*uc).mctx;
                if mctx.is_null() {
                    return (0, 0);
                }
                ((*mctx).ss.pc as usize, (*mctx).ss.fp as usize)
            };
            return (ip, fp);
        }

        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            #[repr(C)]
            struct X86ThreadState {
                _rax: u64,
                _rbx: u64,
                _rcx: u64,
                _rdx: u64,
                _rdi: u64,
                _rsi: u64,
                rbp: u64,
                _rsp: u64,
                _r8: u64,
                _r9: u64,
                _r10: u64,
                _r11: u64,
                _r12: u64,
                _r13: u64,
                _r14: u64,
                _r15: u64,
                rip: u64,
                _rflags: u64,
                _cs: u64,
                _fs: u64,
                _gs: u64,
            }
            #[repr(C)]
            struct MachMcontext {
                _es: [u8; 16], // __darwin_x86_exception_state64
                ss: X86ThreadState,
            }
            #[repr(C)]
            struct DarwinUcontext {
                _onstack: i32,
                _sigmask: u32,
                _stack: libc::stack_t,
                _link: *mut libc::c_void,
                _mcsize: usize,
                mctx: *const MachMcontext,
            }
            let (ip, fp) = unsafe {
                let uc = ctx as *const DarwinUcontext;
                let mctx = (*uc).mctx;
                if mctx.is_null() {
                    return (0, 0);
                }
                ((*mctx).ss.rip as usize, (*mctx).ss.rbp as usize)
            };
            return (ip, fp);
        }

        // Unsupported platform — no frames.
        #[allow(unreachable_code)]
        (0, 0)
    }

    /// Walk the frame-pointer chain, collecting return addresses.
    ///
    /// Fully async-signal-safe: only raw pointer reads, no library calls.
    /// Stops at the first invalid (null, misaligned, or suspiciously small)
    /// frame pointer.
    unsafe fn walk_frame_pointers(initial_fp: usize, out: &mut [usize], max: usize) -> usize {
        let mut fp = initial_fp;
        let mut count = 0;

        while count < max {
            // Validate: non-null, pointer-aligned, not in the zero page.
            if fp == 0 || fp < 4096 || !fp.is_multiple_of(core::mem::size_of::<usize>()) {
                break;
            }
            // On both x86_64 and aarch64, the frame layout is:
            //   [fp+0] = previous frame pointer
            //   [fp+8] = return address
            let prev_fp = unsafe { *(fp as *const usize) };
            let ret_addr = unsafe { *((fp + core::mem::size_of::<usize>()) as *const usize) };

            if ret_addr == 0 || ret_addr < 4096 {
                break;
            }
            out[count] = ret_addr;
            count += 1;

            // Frame pointer must move upward (toward higher addresses on
            // most architectures) to avoid infinite loops.
            if prev_fp <= fp {
                break;
            }
            fp = prev_fp;
        }

        count
    }

    /// File descriptor for the pre-opened crash file.
    static CRASH_FD: AtomicI32 = AtomicI32::new(-1);

    /// Pre-allocated write buffer (lives in .bss, zero cost when not crashing).
    static mut CRASH_BUF: [u8; MAX_FILE_SIZE] = [0; MAX_FILE_SIZE];

    /// Saved original terminal state for restoration in the signal handler.
    static mut ORIGINAL_TERMIOS: libc::termios = unsafe { std::mem::zeroed() };

    /// Whether we successfully saved the original termios.
    static mut HAS_TERMIOS: bool = false;

    /// Application version string, set at install time.
    static mut APP_VERSION: [u8; format::VERSION_STRING_LEN] = [0; format::VERSION_STRING_LEN];

    /// Alternate signal stack memory (16 KiB via mmap).
    const ALT_STACK_SIZE: usize = 16 * 1024;

    /// Guards against double-allocating the alternate signal stack when
    /// [`install_terminal_restore_only`] is followed by [`install`].
    static ALT_STACK_INSTALLED: AtomicBool = AtomicBool::new(false);

    /// Save the current terminal state for restoration in signal handlers.
    fn save_termios() {
        unsafe {
            let termios = &mut *std::ptr::addr_of_mut!(ORIGINAL_TERMIOS);
            if libc::tcgetattr(0, termios) == 0 {
                *std::ptr::addr_of_mut!(HAS_TERMIOS) = true;
            }
        }
    }

    /// Allocate an alternate signal stack via mmap (survives stack overflow).
    ///
    /// No-op if already installed (idempotent across
    /// [`install_terminal_restore_only`] → [`install`] sequences).
    fn setup_alt_stack() {
        if ALT_STACK_INSTALLED.swap(true, Ordering::AcqRel) {
            return;
        }
        unsafe {
            let stack_mem = libc::mmap(
                std::ptr::null_mut(),
                ALT_STACK_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if stack_mem != libc::MAP_FAILED {
                let ss = libc::stack_t {
                    ss_sp: stack_mem,
                    ss_flags: 0,
                    ss_size: ALT_STACK_SIZE,
                };
                libc::sigaltstack(&ss, std::ptr::null_mut());
            }
        }
    }

    /// Restore termios and re-raise. No escape codes.
    ///
    /// # Safety
    ///
    /// Must only be called from a signal handler context.
    unsafe fn restore_termios_and_reraise(sig: libc::c_int) {
        unsafe {
            if *std::ptr::addr_of!(HAS_TERMIOS) {
                libc::tcsetattr(0, libc::TCSANOW, std::ptr::addr_of!(ORIGINAL_TERMIOS));
            }
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            sa.sa_flags = 0;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    /// Restore terminal escape codes + termios, then re-raise.
    ///
    /// # Safety
    ///
    /// Must only be called from a signal handler context.
    unsafe fn restore_terminal_and_reraise(sig: libc::c_int) {
        unsafe {
            terminal::restore_in_signal_handler();
            restore_termios_and_reraise(sig);
        }
    }

    /// Register a signal handler for SIGBUS, SIGSEGV, and SIGABRT.
    ///
    /// SIGABRT is hooked so `panic = "abort"` deaths (every Rust panic in
    /// release builds) produce a crash report instead of a bare `Aborted`.
    ///
    /// Flags: `SA_SIGINFO | SA_ONSTACK | SA_RESETHAND`. `SA_RESETHAND`
    /// resets disposition to `SIG_DFL` after delivery, preventing recursive
    /// faults in the handler from looping. The handlers additionally restore
    /// `SIG_DFL` and re-raise explicitly, so the process still terminates
    /// with the original signal's semantics (exit status, core dumps).
    ///
    /// # Safety
    ///
    /// `handler` must be a valid `sa_sigaction`-compatible function pointer.
    unsafe fn register_crash_signals(
        handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    ) {
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut sa.sa_mask);

            libc::sigaction(libc::SIGBUS, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGABRT, &sa, std::ptr::null_mut());
        }
    }

    /// Minimal handler: restore termios only (no escape codes), then re-raise.
    unsafe extern "C" fn terminal_restore_handler_basic(
        sig: libc::c_int,
        _info: *mut libc::siginfo_t,
        _ctx: *mut libc::c_void,
    ) {
        unsafe {
            restore_termios_and_reraise(sig);
        }
    }

    /// Minimal handler: restore escape codes + termios, then re-raise.
    unsafe extern "C" fn terminal_restore_handler(
        sig: libc::c_int,
        _info: *mut libc::siginfo_t,
        _ctx: *mut libc::c_void,
    ) {
        unsafe {
            restore_terminal_and_reraise(sig);
        }
    }

    /// Write crash blob to the pre-opened fd. Shared by crash handler variants.
    ///
    /// # Safety
    ///
    /// Signal handler context. Only async-signal-safe operations.
    unsafe fn write_crash_blob(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        ctx: *mut libc::c_void,
    ) {
        unsafe {
            let fd = CRASH_FD.load(Ordering::Relaxed);
            if fd >= 0 {
                let si_code = if !info.is_null() { (*info).si_code } else { 0 };

                #[cfg(target_os = "macos")]
                let si_addr = if !info.is_null() {
                    (*info).si_addr as u64
                } else {
                    0
                };
                #[cfg(target_os = "linux")]
                let si_addr = if !info.is_null() {
                    (*info).si_addr() as u64
                } else {
                    0
                };
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                let si_addr: u64 = 0;

                let pid = libc::getpid() as u32;
                let timestamp = libc::time(std::ptr::null_mut()) as u64;

                let mut frames: [usize; MAX_FRAMES] = [0; MAX_FRAMES];
                let mut n_frames: u16 = 0;
                let buf = &mut *std::ptr::addr_of_mut!(CRASH_BUF);
                let version = &*std::ptr::addr_of!(APP_VERSION);

                let (crash_pc, crash_fp) = extract_pc_and_fp(ctx);
                if crash_pc != 0 {
                    frames[0] = crash_pc;
                    n_frames = 1;
                }

                // Write the blob with the crash PC before walking frames.
                // Frame walking dereferences arbitrary pointers and can fault;
                // SA_RESETHAND would kill us without writing anything.
                let mut offset = format::writer::write_header(
                    buf, sig as u8, si_code, si_addr, pid, timestamp, n_frames, version,
                );
                for frame in frames.iter().take(n_frames as usize) {
                    offset = format::writer::write_frame(buf, offset, *frame);
                }
                libc::write(fd, buf.as_ptr() as *const libc::c_void, offset);

                // Best-effort: walk frame pointers for additional context.
                // If this faults, the 1-frame blob above is already on disk.
                if crash_fp != 0 && crash_pc != 0 {
                    let walked = walk_frame_pointers(crash_fp, &mut frames[1..], MAX_FRAMES - 1);
                    if walked > 0 {
                        n_frames += walked as u16;
                        let mut offset = format::writer::write_header(
                            buf, sig as u8, si_code, si_addr, pid, timestamp, n_frames, version,
                        );
                        for frame in frames.iter().take(n_frames as usize) {
                            offset = format::writer::write_frame(buf, offset, *frame);
                        }
                        libc::lseek(fd, 0, libc::SEEK_SET);
                        libc::write(fd, buf.as_ptr() as *const libc::c_void, offset);
                    }
                }

                CRASH_FD.store(-1, Ordering::Relaxed);
                libc::close(fd);
            }
        }
    }

    /// Crash handler: blob + termios only (no escape codes).
    unsafe extern "C" fn crash_handler_basic(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        ctx: *mut libc::c_void,
    ) {
        unsafe {
            libc::alarm(3);
            write_crash_blob(sig, info, ctx);
            restore_termios_and_reraise(sig);
        }
    }

    /// Crash handler: blob + escape codes + termios.
    unsafe extern "C" fn crash_handler(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        ctx: *mut libc::c_void,
    ) {
        unsafe {
            libc::alarm(3);
            write_crash_blob(sig, info, ctx);
            restore_terminal_and_reraise(sig);
        }
    }

    /// Install a minimal SIGSEGV/SIGBUS/SIGABRT handler that restores termios
    /// on crash.
    ///
    /// Does NOT write terminal escape codes — call
    /// [`enable_terminal_escape_restore`] after TUI modes are enabled.
    ///
    /// If [`install`] is called later, it replaces these handlers.
    pub fn install_terminal_restore_only() {
        save_termios();
        setup_alt_stack();
        unsafe { register_crash_signals(terminal_restore_handler_basic) };
    }

    /// Install the crash handler. Must be called early in `main()`, before any
    /// terminal initialization or async runtime setup.
    pub fn install(crash_dir: &Path, echo_agent_version: &str) -> bool {
        let crash_file = crash_dir.join("last-crash.bin");

        // Create the crash directory if it doesn't exist.
        if std::fs::create_dir_all(crash_dir).is_err() {
            return false;
        }

        // Open crash file (pre-opened fd for the signal handler).
        let c_path = match CString::new(crash_file.as_os_str().as_bytes()) {
            Ok(p) => p,
            Err(_) => return false,
        };
        // Owner-only: crash blobs hold stack IPs / fault addresses.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o600,
            )
        };
        if fd < 0 {
            return false;
        }
        // open's mode is create-only; tighten upgrades of older 0644 blobs.
        if unsafe { libc::fchmod(fd, 0o600) } != 0 {
            unsafe {
                libc::close(fd);
            }
            return false;
        }
        CRASH_FD.store(fd, Ordering::Relaxed);

        // Store version string.
        unsafe {
            let version = &mut *std::ptr::addr_of_mut!(APP_VERSION);
            version.fill(0);
            let copy_len = echo_agent_version.len().min(format::VERSION_STRING_LEN);
            version[..copy_len].copy_from_slice(&echo_agent_version.as_bytes()[..copy_len]);
        }

        save_termios();
        setup_alt_stack();
        unsafe { register_crash_signals(crash_handler_basic) };

        true
    }

    /// Upgrade SIGSEGV/SIGBUS/SIGABRT handlers to include terminal escape
    /// code restoration. Call when TUI modes are enabled.
    pub fn enable_terminal_escape_restore() {
        unsafe {
            register_crash_signals(if CRASH_FD.load(Ordering::Relaxed) >= 0 {
                crash_handler
            } else {
                terminal_restore_handler
            });
        }
    }

    /// Downgrade SIGSEGV/SIGBUS/SIGABRT handlers to termios-only restoration.
    /// Call when TUI modes are disabled.
    pub fn disable_terminal_escape_restore() {
        unsafe {
            register_crash_signals(if CRASH_FD.load(Ordering::Relaxed) >= 0 {
                crash_handler_basic
            } else {
                terminal_restore_handler_basic
            });
        }
    }
}

#[cfg(unix)]
pub use imp::{
    disable_terminal_escape_restore, enable_terminal_escape_restore, install,
    install_terminal_restore_only,
};

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::atomic::{AtomicPtr, Ordering};

    use crate::format::{self, MAX_FILE_SIZE, MAX_FRAMES};

    static CRASH_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static mut CRASH_BUF: [u8; MAX_FILE_SIZE] = [0; MAX_FILE_SIZE];
    static mut APP_VERSION: [u8; format::VERSION_STRING_LEN] = [0; format::VERSION_STRING_LEN];

    const EXCEPTION_ACCESS_VIOLATION: i32 = 0xC0000005_u32 as i32;
    const EXCEPTION_STACK_OVERFLOW: i32 = 0xC00000FD_u32 as i32;
    const EXCEPTION_IN_PAGE_ERROR: i32 = 0xC0000006_u32 as i32;
    const EXCEPTION_ILLEGAL_INSTRUCTION: i32 = 0xC000001D_u32 as i32;
    const EXCEPTION_ARRAY_BOUNDS_EXCEEDED: i32 = 0xC000008C_u32 as i32;
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    // CreateFileW constants.
    const GENERIC_WRITE: u32 = 0x40000000;
    const CREATE_ALWAYS: u32 = 2;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x00000080;
    const FILE_BEGIN: u32 = 0;

    /// Walk the frame-pointer chain, collecting return addresses.
    ///
    /// [fp+0] = previous frame pointer, [fp+8] = return address.
    /// Stops at null, misaligned, or non-ascending frame pointers.
    unsafe fn walk_frame_pointers(initial_fp: usize, out: &mut [usize], max: usize) -> usize {
        let mut fp = initial_fp;
        let mut count = 0;

        while count < max {
            if fp == 0 || fp < 4096 || !fp.is_multiple_of(core::mem::size_of::<usize>()) {
                break;
            }
            let prev_fp = unsafe { *(fp as *const usize) };
            let ret_addr = unsafe { *((fp + core::mem::size_of::<usize>()) as *const usize) };

            if ret_addr == 0 || ret_addr < 4096 {
                break;
            }
            out[count] = ret_addr;
            count += 1;

            if prev_fp <= fp {
                break;
            }
            fp = prev_fp;
        }

        count
    }

    /// Map Windows exception code to a Unix signal number for the blob format.
    fn exception_to_signal(code: i32) -> u8 {
        match code {
            EXCEPTION_IN_PAGE_ERROR => 7,       // SIGBUS
            EXCEPTION_ILLEGAL_INSTRUCTION => 4, // SIGILL
            _ => 11,                            // SIGSEGV
        }
    }

    /// Whether the exception code is a fatal memory/instruction fault that
    /// warrants crash handling.
    fn is_fatal_exception(code: i32) -> bool {
        matches!(
            code,
            EXCEPTION_ACCESS_VIOLATION
                | EXCEPTION_STACK_OVERFLOW
                | EXCEPTION_IN_PAGE_ERROR
                | EXCEPTION_ILLEGAL_INSTRUCTION
                | EXCEPTION_ARRAY_BOUNDS_EXCEEDED
        )
    }

    unsafe extern "system" fn crash_handler(
        info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        unsafe {
            if info.is_null() {
                return EXCEPTION_CONTINUE_SEARCH;
            }

            let exception_record = (*info).ExceptionRecord;
            let context_record = (*info).ContextRecord;
            if exception_record.is_null() || context_record.is_null() {
                return EXCEPTION_CONTINUE_SEARCH;
            }

            let exception_code = (*exception_record).ExceptionCode;
            if !is_fatal_exception(exception_code) {
                return EXCEPTION_CONTINUE_SEARCH;
            }

            let handle = CRASH_HANDLE.load(Ordering::Relaxed);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return EXCEPTION_CONTINUE_SEARCH;
            }

            let signal = exception_to_signal(exception_code);
            let si_code = exception_code as i32;

            // ExceptionInformation[1] holds the faulting address for ACCESS_VIOLATION.
            let si_addr = if exception_code == EXCEPTION_ACCESS_VIOLATION
                && (*exception_record).NumberParameters >= 2
            {
                (*exception_record).ExceptionInformation[1] as u64
            } else {
                0
            };

            let pid = windows_sys::Win32::System::Threading::GetCurrentProcessId();

            let mut ft = windows_sys::Win32::Foundation::FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut ft);
            let win_ticks = (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64;
            // FILETIME epoch (1601) → Unix epoch (1970): 116444736000000000 100ns ticks.
            let timestamp = win_ticks.saturating_sub(116_444_736_000_000_000) / 10_000_000;

            let mut frames: [usize; MAX_FRAMES] = [0; MAX_FRAMES];
            let mut n_frames: u16 = 0;
            let buf = &mut *std::ptr::addr_of_mut!(CRASH_BUF);
            let version = &*std::ptr::addr_of!(APP_VERSION);

            #[cfg(target_arch = "x86_64")]
            let (crash_pc, crash_fp) = (
                (*context_record).Rip as usize,
                (*context_record).Rbp as usize,
            );
            // ARM64 Windows: capture PC only; frame-pointer walking is
            // unreliable without verifying the exact windows-sys CONTEXT layout.
            #[cfg(not(target_arch = "x86_64"))]
            let (crash_pc, crash_fp) = (0usize, 0usize);

            if crash_pc != 0 {
                frames[0] = crash_pc;
                n_frames = 1;
            }

            // Write crash PC blob first (frame walking can fault).
            let mut offset = format::writer::write_header(
                buf, signal, si_code, si_addr, pid, timestamp, n_frames, version,
            );
            for frame in frames.iter().take(n_frames as usize) {
                offset = format::writer::write_frame(buf, offset, *frame);
            }
            write_to_handle(handle, buf, offset);

            // Best-effort: walk frame pointers for a full backtrace.
            if crash_fp != 0 && crash_pc != 0 {
                let walked = walk_frame_pointers(crash_fp, &mut frames[1..], MAX_FRAMES - 1);
                if walked > 0 {
                    n_frames += walked as u16;
                    let mut offset = format::writer::write_header(
                        buf, signal, si_code, si_addr, pid, timestamp, n_frames, version,
                    );
                    for frame in frames.iter().take(n_frames as usize) {
                        offset = format::writer::write_frame(buf, offset, *frame);
                    }
                    windows_sys::Win32::Storage::FileSystem::SetFilePointer(
                        handle,
                        0,
                        std::ptr::null_mut(),
                        FILE_BEGIN,
                    );
                    write_to_handle(handle, buf, offset);
                }
            }

            CRASH_HANDLE.store(std::ptr::null_mut(), Ordering::Relaxed);
            windows_sys::Win32::Foundation::CloseHandle(handle);

            EXCEPTION_CONTINUE_SEARCH
        }
    }

    /// Crash handler with escape code restoration (TUI active).
    unsafe extern "system" fn crash_handler_with_terminal(
        info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        let result = unsafe { crash_handler(info) };
        crate::terminal::restore_in_signal_handler();
        result
    }

    unsafe fn write_to_handle(handle: *mut c_void, buf: &[u8], len: usize) {
        let mut written: u32 = 0;
        unsafe {
            windows_sys::Win32::Storage::FileSystem::WriteFile(
                handle,
                buf.as_ptr(),
                len as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }

    /// Minimal exception filter: no-op (no escape codes, no crash reporting).
    unsafe extern "system" fn terminal_restore_filter_basic(
        _info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Minimal exception filter: restore terminal escape codes (TUI active).
    unsafe extern "system" fn terminal_restore_filter(
        info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
    ) -> i32 {
        unsafe {
            if info.is_null() {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            let exception_record = (*info).ExceptionRecord;
            if exception_record.is_null() {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            let exception_code = (*exception_record).ExceptionCode;
            if !is_fatal_exception(exception_code) {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            crate::terminal::restore_in_signal_handler();
            EXCEPTION_CONTINUE_SEARCH
        }
    }

    pub fn install_terminal_restore_only() {
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                terminal_restore_filter_basic,
            ));
        }
    }

    pub fn install(crash_dir: &Path, echo_agent_version: &str) -> bool {
        use std::os::windows::ffi::OsStrExt;

        let crash_file = crash_dir.join("last-crash.bin");

        if std::fs::create_dir_all(crash_dir).is_err() {
            return false;
        }

        let wide_path: Vec<u16> = crash_file
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                wide_path.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };

        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        CRASH_HANDLE.store(handle, Ordering::Relaxed);

        unsafe {
            let version = &mut *std::ptr::addr_of_mut!(APP_VERSION);
            version.fill(0);
            let copy_len = echo_agent_version.len().min(format::VERSION_STRING_LEN);
            version[..copy_len].copy_from_slice(&echo_agent_version.as_bytes()[..copy_len]);
        }

        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                crash_handler,
            ));
        }

        true
    }

    pub fn enable_terminal_escape_restore() {
        unsafe {
            let filter = if !CRASH_HANDLE.load(Ordering::Relaxed).is_null() {
                crash_handler_with_terminal
            } else {
                terminal_restore_filter
            };
            windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                filter,
            ));
        }
    }

    pub fn disable_terminal_escape_restore() {
        unsafe {
            let filter = if !CRASH_HANDLE.load(Ordering::Relaxed).is_null() {
                crash_handler
            } else {
                terminal_restore_filter_basic
            };
            windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                filter,
            ));
        }
    }
}

#[cfg(windows)]
pub use win::{
    disable_terminal_escape_restore, enable_terminal_escape_restore, install,
    install_terminal_restore_only,
};

#[cfg(not(any(unix, windows)))]
pub fn install(_crash_dir: &std::path::Path, _app_version: &str) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
pub fn install_terminal_restore_only() {}

#[cfg(not(any(unix, windows)))]
pub fn enable_terminal_escape_restore() {}

#[cfg(not(any(unix, windows)))]
pub fn disable_terminal_escape_restore() {}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Mutex;

    // SIGSEGV/SIGBUS/SIGABRT handlers are process-global. Tests in this binary run on
    // parallel threads, so any two tests that install/read these handlers race.
    // Serialize them through this lock (poison-tolerant: a real assertion
    // failure in one test must not cascade into the other).
    static SIGNAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn install_terminal_restore_only_registers_handlers() {
        let _guard = SIGNAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::install_terminal_restore_only();
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();

            assert_eq!(libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sa), 0);
            assert_ne!(
                sa.sa_sigaction,
                libc::SIG_DFL,
                "SIGSEGV handler should not be SIG_DFL after install"
            );
            assert_ne!(
                sa.sa_flags & libc::SA_ONSTACK,
                0,
                "SIGSEGV handler must use alternate signal stack"
            );
            // Note: SA_RESETHAND is set in our sigaction call but macOS XNU
            // does not round-trip it through the sigaction query — the kernel
            // stores it in ps_sigreset internally but returns sa_flags=0x41
            // (SA_SIGINFO|SA_ONSTACK only). The flag IS honored for signal
            // delivery. Verified via the integration test
            // `sigsegv_produces_valid_crash_blob` which relies on SA_RESETHAND
            // to re-raise with SIG_DFL after the handler runs.

            assert_eq!(libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut sa), 0);
            assert_ne!(
                sa.sa_sigaction,
                libc::SIG_DFL,
                "SIGBUS handler should not be SIG_DFL after install"
            );
            assert_ne!(
                sa.sa_flags & libc::SA_ONSTACK,
                0,
                "SIGBUS handler must use alternate signal stack"
            );

            assert_eq!(libc::sigaction(libc::SIGABRT, std::ptr::null(), &mut sa), 0);
            assert_ne!(
                sa.sa_sigaction,
                libc::SIG_DFL,
                "SIGABRT handler should not be SIG_DFL after install"
            );
            assert_ne!(
                sa.sa_flags & libc::SA_ONSTACK,
                0,
                "SIGABRT handler must use alternate signal stack"
            );
        }
    }

    #[test]
    fn full_install_replaces_minimal_handler() {
        let _guard = SIGNAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::install_terminal_restore_only();

        let handler_before = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sa);
            sa.sa_sigaction
        };

        let dir = std::env::temp_dir().join("echo-agent-crash-handler-test-replace");
        let _ = std::fs::create_dir_all(&dir);
        super::install(&dir, "test-version");

        let handler_after = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sa);
            sa.sa_sigaction
        };

        assert_ne!(
            handler_after, handler_before,
            "full install should replace the minimal handler"
        );
    }

    #[test]
    fn install_creates_owner_only_crash_blob() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = SIGNAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "echo-agent-crash-handler-test-0600-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create crash dir");

        assert!(super::install(&dir, "test-version"));
        let path = dir.join("last-crash.bin");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "new last-crash.bin must be owner-only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_tightens_preexisting_0644_crash_blob() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = SIGNAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "echo-agent-crash-handler-test-tighten-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create crash dir");

        let path = dir.join("last-crash.bin");
        std::fs::write(&path, b"old").expect("seed");
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).expect("set 0644");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
            0o644
        );

        assert!(super::install(&dir, "test-version"));
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "install must fchmod preexisting 0644 blobs to owner-only"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
