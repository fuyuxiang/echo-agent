//! Windows system sleep/wake via `PowerRegisterSuspendResumeNotification`
//! with a `DEVICE_NOTIFY_CALLBACK` recipient — no hidden window or message
//! loop required (Windows 8+).
//!
//! NOTE: this module only compiles when targeting Windows.

use std::os::raw::c_void;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, PowerRegisterSuspendResumeNotification,
    PowerUnregisterSuspendResumeNotification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

use super::{PowerCallback, PowerEvent};

// Power-broadcast event types (WM_POWERBROADCAST `wParam`).
const PBT_APMSUSPEND: u32 = 0x0004;
const PBT_APMRESUMESUSPEND: u32 = 0x0007;
const PBT_APMRESUMEAUTOMATIC: u32 = 0x0012;

const ERROR_SUCCESS: u32 = 0;

/// Heap-pinned so its address stays stable for the registration lifetime; the
/// raw pointer is handed to the OS as the callback context.
struct Context {
    callback: PowerCallback,
}

pub(crate) struct Listener {
    // Registration handle from `PowerRegisterSuspendResumeNotification`
    // (a `*mut c_void`; cast to `HPOWERNOTIFY` for unregister).
    handle: *mut c_void,
    // Kept alive (and freed in `Drop`) because the OS holds a raw pointer to it.
    ctx: *mut Context,
}

// The OS invokes the callback on an arbitrary thread; the handle is only used
// to unregister. `PowerCallback` is `Send + Sync`.
unsafe impl Send for Listener {}
unsafe impl Sync for Listener {}

impl Listener {
    pub(crate) fn start(callback: PowerCallback) -> Option<Self> {
        let ctx = Box::into_raw(Box::new(Context { callback }));

        let mut params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(power_callback),
            Context: ctx as *mut c_void,
        };

        let mut handle: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                &mut params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as HANDLE,
                &mut handle,
            )
        };

        if status != ERROR_SUCCESS || handle.is_null() {
            unsafe { drop(Box::from_raw(ctx)) };
            return None;
        }

        Some(Self { handle, ctx })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        unsafe {
            PowerUnregisterSuspendResumeNotification(self.handle as HPOWERNOTIFY);
            drop(Box::from_raw(self.ctx));
        }
    }
}

unsafe extern "system" fn power_callback(
    context: *const c_void,
    event_type: u32,
    _setting: *const c_void,
) -> u32 {
    // Safe: `context` is the live `Context` we registered with.
    let ctx = unsafe { &*(context as *const Context) };
    match event_type {
        PBT_APMSUSPEND => (ctx.callback)(PowerEvent::WillSleep),
        // A single resume can deliver both PBT_APMRESUMEAUTOMATIC and
        // PBT_APMRESUMESUSPEND, so `DidWake` may fire twice per wake. That is
        // fine and intentional: lowering the sleep gate is idempotent, so a
        // duplicate wake is harmless — do not try to "dedupe" this later.
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => (ctx.callback)(PowerEvent::DidWake),
        _ => {}
    }
    ERROR_SUCCESS
}

pub(crate) fn current_power_state() -> crate::PowerState {
    // No synchronous dark-wake query wired up on Windows; report Unknown so
    // callers fall back to the suspend/resume notification path.
    crate::PowerState::Unknown
}

/// No power-assertion support on this platform: callers carry on unprotected,
/// which is the same behaviour as before assertions existed.
///
/// Never constructed here (`hold_awake` always returns `None`); it exists so
/// the cross-platform `SleepAssertion` has a field type on every target.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct Assertion;

pub(crate) fn hold_awake(_reason: &str) -> Option<Assertion> {
    None
}
