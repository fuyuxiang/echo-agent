//! Session resume, persistence, storage recovery, background-task cleanup, and
//! parked/wake lifecycle PTY coverage.
//!
//! All cases are ignored for ordinary Cargo runs; Bazel opts in and caps this
//! process-heavy family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

#[path = "pty_e2e/background_task_reaped_on_quit.rs"]
mod background_task_reaped_on_quit;
#[path = "pty_e2e/continue_resumes_session_with_history.rs"]
mod continue_resumes_session_with_history;
#[path = "pty_e2e/endline_park_is_markerless.rs"]
mod endline_park_is_markerless;
#[path = "pty_e2e/endline_wakeups_close_with_markers.rs"]
mod endline_wakeups_close_with_markers;
#[path = "pty_e2e/rename_title_shows_in_prompt_border.rs"]
mod rename_title_shows_in_prompt_border;
#[path = "pty_e2e/reparked_wait_stays_markerless.rs"]
mod reparked_wait_stays_markerless;
#[path = "pty_e2e/spinner_reappears_after_wait_resumes.rs"]
mod spinner_reappears_after_wait_resumes;
#[path = "pty_e2e/storage_upload_parks_on_401_and_drains_after_recovery.rs"]
mod storage_upload_parks_on_401_and_drains_after_recovery;
