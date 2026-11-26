#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::sync::atomic::{AtomicBool, Ordering};

static SIGNAL_HANDLED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
fn set_errno(value: libc::c_int) {
    // SAFETY: __errno_location returns the calling thread's valid errno pointer.
    unsafe { *libc::__errno_location() = value };
}

#[cfg(target_os = "linux")]
fn get_errno() -> libc::c_int {
    // SAFETY: __errno_location returns the calling thread's valid errno pointer.
    unsafe { *libc::__errno_location() }
}

#[cfg(target_os = "macos")]
fn set_errno(value: libc::c_int) {
    // SAFETY: __error returns the calling thread's valid errno pointer.
    unsafe { *libc::__error() = value };
}

#[cfg(target_os = "macos")]
fn get_errno() -> libc::c_int {
    // SAFETY: __error returns the calling thread's valid errno pointer.
    unsafe { *libc::__error() }
}

#[test]
fn signal_dispatch_preserves_errno() {
    SIGNAL_HANDLED.store(false, Ordering::SeqCst);

    // SAFETY: The callback only updates thread-local errno and a lock-free atomic.
    let handler = unsafe {
        signal_hook::low_level::register(libc::SIGUSR2, || {
            set_errno(libc::EAGAIN);
            SIGNAL_HANDLED.store(true, Ordering::SeqCst);
        })
    }
    .expect("register SIGUSR2 handler");

    set_errno(libc::EINTR);
    // SAFETY: pthread_self returns the current valid thread and SIGUSR2 is registered above.
    let result = unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGUSR2) };
    assert_eq!(result, 0);
    let observed_errno = get_errno();
    assert!(signal_hook::low_level::unregister(handler));

    assert!(SIGNAL_HANDLED.load(Ordering::SeqCst));
    assert_eq!(observed_errno, libc::EINTR);
}
