//! Compile the production recovery/guard code without the embedded model runtime.
//! This validates platform API types; it does not substitute for a graphical
//! WebView2 fault-injection run on Windows.
#![allow(dead_code)]
#[path = "../../src-tauri/src/desktop_tray_model.rs"]
mod desktop_tray_model;
#[path = "../../src-tauri/src/desktop_tray.rs"]
mod desktop_tray;
fn show_main_window(_: &tauri::AppHandle) {}
fn request_graceful_exit(_: tauri::AppHandle) {}
extern crate self as echo_agent_tools;
#[path = "../../vendor/echo-agent-build/crates/codegen/echo-agent-tools/src/util/process_safety.rs"]
pub mod process_safety;
pub mod util {
    pub use crate::process_safety;
}
#[path = "../../src-tauri/src/host_watchdog.rs"]
mod host_watchdog;
#[path = "../../src-tauri/src/native_review_scope.rs"]
mod native_review_scope;
#[path = "../../src-tauri/src/process_identity.rs"]
mod process_identity;
#[path = "../../src-tauri/src/process_supervisor.rs"]
mod process_supervisor;
#[path = "../../src-tauri/src/recording_recovery.rs"]
mod recording_recovery;
#[path = "../../src-tauri/src/recovery_policy.rs"]
mod recovery_policy;
#[path = "../../src-tauri/src/webview_recovery.rs"]
mod webview_recovery;
static EXIT_IN_PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static EXIT_LISTENER_READY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
mod meeting_minutes {
    pub fn recover_renderer_recordings() -> Result<usize, String> {
        Ok(0)
    }
}
fn bind_main_window_lifecycle(_: &tauri::AppHandle, _: &tauri::WebviewWindow) {}
fn save_desktop_window_state(_: &tauri::AppHandle, _: &tauri::WebviewWindow) {}

#[cfg(feature = "native-fixture")]
mod fixture;
#[cfg(feature = "native-fixture")]
pub use fixture::run as run_fixture;

#[cfg(windows)]
mod windows_injection;
