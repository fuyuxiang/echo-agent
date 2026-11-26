//! macOS system sleep/wake via IOKit `IORegisterForSystemPower`.
//!
//! IOKit delivers power notifications through a `CFRunLoop` source, so we run a
//! dedicated thread whose run loop receives the callbacks. The thread owns all
//! IOKit resources for their full lifetime and tears them down after the run
//! loop is stopped (from `Drop`).
//!
//! FFI is declared directly (CoreFoundation + IOKit frameworks) to avoid a
//! `core-foundation` crate dependency for this tiny surface. The opaque CF
//! types (`CFRunLoopRef`, `CFRunLoopSourceRef`, `CFRunLoopMode`) are pointers.

use std::os::raw::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use super::{PowerCallback, PowerEvent, PowerState};

// `io_object_t` / `io_connect_t` are `mach_port_t` == `unsigned int`.
type MachPort = u32;
const MACH_PORT_NULL: MachPort = 0;

// IOKit power-management message types (IOMessage.h).
const K_IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
const K_IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
const K_IO_MESSAGE_SYSTEM_WILL_NOT_SLEEP: u32 = 0xe000_0290;
const K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

// IOPM system-power capability bits (`IOPMCapabilityBits`). These constants and
// the `IOPMConnectionGetSystemCapabilities` query below are **SPI**: declared in
// the *private* `IOPMLibPrivate.h` (IOKitUser), not the public `IOPMLib.h` that
// ships in the SDK. A dark wake has CPU (and usually network/disk) but *not*
// video: the system is up for background maintenance with the display off. A
// full/user wake additionally carries the video capability. (See
// `crate::PowerState` for the canonical dark-wake explanation.)
const K_IOPM_CAPABILITY_CPU: u32 = 0x1;
const K_IOPM_CAPABILITY_VIDEO: u32 = 0x2;

type IoServiceInterestCallback = extern "C" fn(
    refcon: *mut c_void,
    service: MachPort,
    message_type: u32,
    message_argument: *mut c_void,
);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: *const c_void; // CFRunLoopMode (CFStringRef)
    static kCFRunLoopDefaultMode: *const c_void; // CFRunLoopMode (CFStringRef)
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
    fn CFRunLoopStop(rl: *mut c_void);
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut *mut c_void,
        callback: IoServiceInterestCallback,
        notifier: *mut MachPort,
    ) -> MachPort;
    fn IODeregisterForSystemPower(notifier: *mut MachPort) -> i32;
    fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> *mut c_void;
    fn IONotificationPortDestroy(port: *mut c_void);
    fn IOAllowPowerChange(kern_port: MachPort, notification_id: isize) -> i32;
    fn IOServiceClose(connect: MachPort) -> i32;
    // `IOPMCapabilityBits IOPMConnectionGetSystemCapabilities(void)` — an
    // undeclared **SPI** symbol: exported by IOKit but prototyped only in the
    // private `IOPMLibPrivate.h`, not the public SDK. Despite the "Connection"
    // in the name the real prototype takes **no** arguments (it reads global
    // state — no `IOPMConnectionCreate`, no run loop, no acknowledgment), so
    // this zero-arg declaration matches the ABI: a cheap synchronous read of
    // the current power state.
    fn IOPMConnectionGetSystemCapabilities() -> u32;
}

/// Classify raw IOPM capability bits into a coarse [`PowerState`].
///
/// - no CPU bit  → [`PowerState::Unknown`]: we only ever call this while the
///   process is executing, so a missing CPU bit is a transitional / bogus
///   sample. Fail open so callers keep their existing behavior rather than
///   blocking on a bad read.
/// - CPU + video → [`PowerState::FullWake`].
/// - CPU, no video → [`PowerState::DarkWake`].
///
/// Note an idle *display sleep* while the system is otherwise fully awake keeps
/// the system-level video capability set (the system can drive graphics on
/// demand), so it classifies as `FullWake`, not `DarkWake` — only a real dark
/// wake from sleep drops the video capability.
fn classify_capabilities(caps: u32) -> PowerState {
    if caps & K_IOPM_CAPABILITY_CPU == 0 {
        return PowerState::Unknown;
    }
    if caps & K_IOPM_CAPABILITY_VIDEO != 0 {
        PowerState::FullWake
    } else {
        PowerState::DarkWake
    }
}

pub(crate) fn current_power_state() -> PowerState {
    // Safe: the C function takes no arguments and returns a plain bitfield.
    let caps = unsafe { IOPMConnectionGetSystemCapabilities() };
    // IOKit also exports `IOPMIsADarkWake(IOPMCapabilityBits)` /
    // `IOPMIsAUserWake(IOPMCapabilityBits)` (also `IOPMLibPrivate.h` SPI), which
    // classify these bits directly. We classify them ourselves so the mapping
    // stays a pure, unit-tested function (`classify_capabilities`) and so we
    // control the fail-open-to-`Unknown` behavior on a missing CPU bit, which
    // those predicates don't express.
    classify_capabilities(caps)
}

/// Lives for the duration of the run loop; pointed to by the IOKit `refcon`.
/// Only touched from the run-loop thread (registration sets `root_port`
/// before the loop runs; the callback reads both fields on that same thread).
struct Context {
    callback: PowerCallback,
    root_port: MachPort,
}

/// `CFRunLoopRef` is safe to call `CFRunLoopStop` on from another thread.
struct SendRunLoop(*mut c_void);
unsafe impl Send for SendRunLoop {}

pub(crate) struct Listener {
    runloop: SendRunLoop,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Listener {
    pub(crate) fn start(callback: PowerCallback) -> Option<Self> {
        let (tx, rx) = mpsc::channel::<Option<SendRunLoop>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = thread::Builder::new()
            .name("echo-agent-power-listener".into())
            .spawn(move || run_thread(callback, tx, stop_thread))
            .ok()?;

        // Block until the thread has registered (or failed). This keeps the
        // returned handle meaningful and lets us return `None` on failure.
        match rx.recv() {
            Ok(Some(runloop)) => Some(Self {
                runloop,
                stop,
                handle: Some(handle),
            }),
            _ => {
                let _ = handle.join();
                None
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Signal stop, then wake the run loop so the thread exits promptly and
        // tears down IOKit resources. The stop flag also covers the race where
        // `CFRunLoopStop` arrives before the loop starts (the timed
        // `CFRunLoopRunInMode` re-checks the flag).
        self.stop.store(true, Ordering::SeqCst);
        unsafe { CFRunLoopStop(self.runloop.0) };
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_thread(
    callback: PowerCallback,
    tx: mpsc::Sender<Option<SendRunLoop>>,
    stop: Arc<AtomicBool>,
) {
    let ctx = Box::into_raw(Box::new(Context {
        callback,
        root_port: MACH_PORT_NULL,
    }));

    let mut notifier: MachPort = MACH_PORT_NULL;
    let mut port: *mut c_void = std::ptr::null_mut();
    let root_port = unsafe {
        IORegisterForSystemPower(ctx as *mut c_void, &mut port, power_callback, &mut notifier)
    };

    if root_port == MACH_PORT_NULL || port.is_null() {
        // Registration failed — reclaim the context and report failure.
        unsafe { drop(Box::from_raw(ctx)) };
        let _ = tx.send(None);
        return;
    }
    // Safe: the callback cannot fire until the run loop runs, below.
    unsafe { (*ctx).root_port = root_port };

    let runloop = unsafe { CFRunLoopGetCurrent() };
    unsafe {
        let source = IONotificationPortGetRunLoopSource(port);
        CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
    }

    if tx.send(Some(SendRunLoop(runloop))).is_err() {
        // Receiver gone (start() bailed) — clean up and exit without running.
        unsafe {
            IODeregisterForSystemPower(&mut notifier);
            IONotificationPortDestroy(port);
            IOServiceClose(root_port);
            drop(Box::from_raw(ctx));
        }
        return;
    }

    // Service power notifications until stopped. `Drop` calls `CFRunLoopStop`,
    // which wakes this immediately; the finite (rather than infinite) timeout
    // only exists to cover the rare race where `CFRunLoopStop` arrives before
    // the loop starts. A long interval keeps idle wakeups negligible without
    // delaying normal teardown.
    while !stop.load(Ordering::SeqCst) {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 5.0, 0) };
    }

    // Run loop stopped: tear down IOKit resources and the context.
    unsafe {
        IODeregisterForSystemPower(&mut notifier);
        IONotificationPortDestroy(port);
        IOServiceClose(root_port);
        drop(Box::from_raw(ctx));
    }
}

/// Pure mapping of an IOKit power message to the [`PowerEvent`] delivered to
/// the user callback (if any) and whether the message requires an
/// `IOAllowPowerChange` acknowledgment. Split from [`power_callback`] so the
/// mapping is unit-testable without IOKit ports.
///
/// - `CAN_SYSTEM_SLEEP` (idle-sleep query) maps to [`PowerEvent::WillSleep`]:
///   an idle sleep may follow within seconds, so consumers must treat it
///   exactly like an announced sleep — the auth sleep gate must already be up
///   (and in-flight token refreshes drained, via the bounded blocking callback)
///   *before* we permit the transition. We never veto; the callback runs, then
///   the ack allows the sleep. If the sleep is vetoed by another client,
///   `SYSTEM_WILL_NOT_SLEEP` arrives and maps to [`PowerEvent::DidWake`]
///   (transition cancelled — same "not sleeping anymore" meaning), lowering the
///   gate; if it proceeds, the later `SYSTEM_WILL_SLEEP` re-raises it
///   (idempotent, and its drain-wait finds the in-flight counter already at
///   zero).
/// - `SYSTEM_WILL_NOT_SLEEP` requires no ack (informational).
fn map_power_message(message_type: u32) -> (Option<PowerEvent>, bool) {
    match message_type {
        K_IO_MESSAGE_CAN_SYSTEM_SLEEP => (Some(PowerEvent::WillSleep), true),
        K_IO_MESSAGE_SYSTEM_WILL_SLEEP => (Some(PowerEvent::WillSleep), true),
        K_IO_MESSAGE_SYSTEM_WILL_NOT_SLEEP => (Some(PowerEvent::DidWake), false),
        K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON => (Some(PowerEvent::DidWake), false),
        _ => (None, false),
    }
}

extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: MachPort,
    message_type: u32,
    message_argument: *mut c_void,
) {
    // Safe: `refcon` is the live `Context` for this run-loop thread.
    let ctx = unsafe { &*(refcon as *const Context) };
    let (event, needs_ack) = map_power_message(message_type);
    if let Some(event) = event {
        // For sleep-bound messages the ack is sent only *after* the callback
        // returns: a `WillSleep` handler may block (bounded) waiting for an
        // in-flight token refresh to finish, which intentionally delays the
        // `IOAllowPowerChange` and holds off the suspend. IOKit allows ~30 s
        // per phase before forcing sleep, so a bounded wait is safe. See the
        // `echo_agent_system_power` crate-level callback contract.
        (ctx.callback)(event);
    }
    if needs_ack {
        unsafe { IOAllowPowerChange(ctx.root_port, message_argument as isize) };
    }
}

// ── Power assertions ────────────────────────────────────────────────

// `IOPMAssertionID` is a `uint32_t`; `kIOPMAssertionLevelOn` is 255 and
// `kIOReturnSuccess` is 0 (IOPMLib.h / IOReturn.h).
type IoPmAssertionId = u32;
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
const K_IO_RETURN_SUCCESS: i32 = 0;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// Spelled out because `kIOPMAssertionTypePreventSystemSleep` is a
/// `CFSTR(...)` macro, not an exported symbol — an `extern static` links
/// and then aborts at load ("symbol not found in flat namespace"), which
/// Linux CI can never catch. `PreventSystemSleep` is also the only type
/// that keeps a machine resident in a dark wake
/// (`PreventUserIdleSystemSleep` suppresses only *idle* sleep).
const ASSERTION_TYPE_PREVENT_SYSTEM_SLEEP: &str = "PreventSystemSleep";

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        assertion_level: u32,
        assertion_name: *const c_void,
        assertion_id: *mut IoPmAssertionId,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: IoPmAssertionId) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        num_bytes: isize,
        encoding: u32,
        is_external_representation: u8,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Live `PreventSystemSleep` assertion, released on drop.
#[derive(Debug)]
pub(crate) struct Assertion(IoPmAssertionId);

impl Drop for Assertion {
    fn drop(&mut self) {
        // SAFETY: the id came from a successful `IOPMAssertionCreateWithName`,
        // this type is not `Clone`, and `drop` runs once — so the assertion is
        // released exactly once. Releasing is what keeps a leaked assertion
        // from pinning the machine awake.
        unsafe { IOPMAssertionRelease(self.0) };
    }
}

/// Create a `CFStringRef` from a Rust `&str`. Caller owns it and must `CFRelease`.
fn cf_string(value: &str) -> *const c_void {
    // SAFETY: pointer/length describe a valid UTF-8 slice, read only for the
    // duration of the call — CF copies the bytes into the new string.
    unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            value.as_ptr(),
            value.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
            0,
        )
    }
}

pub(crate) fn hold_awake(reason: &str) -> Option<Assertion> {
    let kind = cf_string(ASSERTION_TYPE_PREVENT_SYSTEM_SLEEP);
    if kind.is_null() {
        return None;
    }
    let name = cf_string(reason);
    if name.is_null() {
        // SAFETY: `kind` is a live CFStringRef we own.
        unsafe { CFRelease(kind) };
        return None;
    }

    let mut id: IoPmAssertionId = 0;
    // SAFETY: both strings are live CFStringRefs and `id` is a valid
    // out-pointer for the duration of the call.
    let rc = unsafe { IOPMAssertionCreateWithName(kind, K_IOPM_ASSERTION_LEVEL_ON, name, &mut id) };
    // The assertion retains what it needs; drop our references either way.
    // SAFETY: we created both and have not released them yet.
    unsafe {
        CFRelease(name);
        CFRelease(kind);
    }
    (rc == K_IO_RETURN_SUCCESS).then_some(Assertion(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Network (0x8) + disk (0x10): the `kIOPMCapabilityNetwork` /
    // `kIOPMCapabilityDisk` bits a real dark/full wake typically also carries.
    // Named here so the classifier inputs mirror real
    // `IOPMConnectionGetSystemCapabilities` samples, not just the CPU/video bits
    // `classify_capabilities` keys on.
    const K_IOPM_CAPABILITY_NETWORK: u32 = 0x8;
    const K_IOPM_CAPABILITY_DISK: u32 = 0x10;

    #[test]
    fn classify_full_wake_has_video() {
        // CPU + video (+ network/disk) => full/user wake.
        let caps = K_IOPM_CAPABILITY_CPU
            | K_IOPM_CAPABILITY_VIDEO
            | K_IOPM_CAPABILITY_NETWORK
            | K_IOPM_CAPABILITY_DISK;
        assert_eq!(classify_capabilities(caps), PowerState::FullWake);
    }

    #[test]
    fn classify_dark_wake_cpu_without_video() {
        // CPU + network/disk but no video => dark wake.
        assert_eq!(
            classify_capabilities(
                K_IOPM_CAPABILITY_CPU | K_IOPM_CAPABILITY_NETWORK | K_IOPM_CAPABILITY_DISK
            ),
            PowerState::DarkWake
        );
        // CPU alone (no video) is still a dark wake.
        assert_eq!(
            classify_capabilities(K_IOPM_CAPABILITY_CPU),
            PowerState::DarkWake
        );
    }

    #[test]
    fn classify_unknown_without_cpu() {
        // No CPU bit while we are running is a bogus/transitional sample: fail
        // open to Unknown so callers keep their existing behavior.
        assert_eq!(classify_capabilities(0), PowerState::Unknown);
        assert_eq!(
            classify_capabilities(K_IOPM_CAPABILITY_VIDEO),
            PowerState::Unknown
        );
    }

    /// Message → (event, needs_ack) contract. The load-bearing rows:
    /// - the idle-sleep *query* must deliver `WillSleep` (raise the auth sleep
    ///   gate / drain in-flight refreshes **before** we allow the transition —
    ///   an idle sleep can follow within seconds, and a one-time-use OIDC
    ///   refresh-token exchange started in that window would straddle it), and
    ///   must still be acked (we never veto);
    /// - a vetoed sleep must deliver `DidWake` so a gate raised at the query
    ///   is lowered instead of blocking refresh for `SLEEP_GATE_MAX`.
    #[test]
    fn map_power_message_matrix() {
        assert_eq!(
            map_power_message(K_IO_MESSAGE_CAN_SYSTEM_SLEEP),
            (Some(PowerEvent::WillSleep), true)
        );
        assert_eq!(
            map_power_message(K_IO_MESSAGE_SYSTEM_WILL_SLEEP),
            (Some(PowerEvent::WillSleep), true)
        );
        assert_eq!(
            map_power_message(K_IO_MESSAGE_SYSTEM_WILL_NOT_SLEEP),
            (Some(PowerEvent::DidWake), false)
        );
        assert_eq!(
            map_power_message(K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON),
            (Some(PowerEvent::DidWake), false)
        );
        // Unrelated messages (e.g. kIOMessageSystemWillPowerOn 0xe0000320)
        // deliver nothing and need no ack.
        assert_eq!(map_power_message(0xe000_0320), (None, false));
    }
}
