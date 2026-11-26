//! FFmpeg PATH probes and the "install ffmpeg" banner for inline video posters.

use crate::prompt_images::InlineMediaInfo;

pub const FFMPEG_HINT_TEXT: &str = "Install ffmpeg to view inline";

/// Latches positives; re-probes negatives so mid-session install recovers posters.
pub fn ffmpeg_available() -> bool {
    #[cfg(test)]
    if let Some(v) = TEST_FFMPEG_OVERRIDE.with(|c| c.get()) {
        return v;
    }

    use std::sync::atomic::{AtomicBool, Ordering};

    static FOUND: AtomicBool = AtomicBool::new(false);
    if FOUND.load(Ordering::Relaxed) {
        return true;
    }

    let available = echo_agent_config::shell::is_command_available("ffmpeg");
    if available {
        FOUND.store(true, Ordering::Relaxed);
    }
    available
}

#[cfg(test)]
thread_local! {
    static TEST_FFMPEG_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_ffmpeg_available_for_test(available: bool) -> FfmpegTestGuard {
    TEST_FFMPEG_OVERRIDE.with(|c| c.set(Some(available)));
    FfmpegTestGuard
}

#[cfg(test)]
pub(crate) struct FfmpegTestGuard;

#[cfg(test)]
impl Drop for FfmpegTestGuard {
    fn drop(&mut self) {
        TEST_FFMPEG_OVERRIDE.with(|c| c.set(None));
    }
}

/// First on-PATH package manager wins; `None` → hint-only banner (no install line).
fn ffmpeg_install_candidates() -> &'static [(&'static str, &'static str)] {
    if cfg!(target_os = "macos") {
        &[("brew", "! brew install ffmpeg")]
    } else if cfg!(target_os = "windows") {
        &[
            ("winget", "! winget install ffmpeg"),
            ("choco", "! choco install ffmpeg"),
            ("scoop", "! scoop install ffmpeg"),
        ]
    } else {
        &[
            ("apt", "! sudo apt install ffmpeg"),
            ("apt-get", "! sudo apt-get install ffmpeg"),
            ("dnf", "! sudo dnf install ffmpeg"),
            ("pacman", "! sudo pacman -S ffmpeg"),
            ("zypper", "! sudo zypper install ffmpeg"),
            ("apk", "! sudo apk add ffmpeg"),
        ]
    }
}

/// Latches positives; re-probes negatives so mid-session PM install recovers the cmd line.
pub fn ffmpeg_install_cmd() -> Option<&'static str> {
    #[cfg(test)]
    if let Some(v) = TEST_FFMPEG_INSTALL_CMD_OVERRIDE.with(|c| c.get()) {
        return v;
    }

    use std::sync::OnceLock;
    static FOUND: OnceLock<&'static str> = OnceLock::new();
    if let Some(cmd) = FOUND.get() {
        return Some(*cmd);
    }
    for (manager, cmd) in ffmpeg_install_candidates() {
        if echo_agent_config::shell::is_command_available(manager) {
            let _ = FOUND.set(*cmd);
            return Some(*cmd);
        }
    }
    None
}

#[cfg(test)]
thread_local! {
    static TEST_FFMPEG_INSTALL_CMD_OVERRIDE: std::cell::Cell<Option<Option<&'static str>>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_ffmpeg_install_cmd_for_test(
    cmd: Option<&'static str>,
) -> FfmpegInstallCmdTestGuard {
    TEST_FFMPEG_INSTALL_CMD_OVERRIDE.with(|c| c.set(Some(cmd)));
    FfmpegInstallCmdTestGuard
}

#[cfg(test)]
pub(crate) struct FfmpegInstallCmdTestGuard;

#[cfg(test)]
impl Drop for FfmpegInstallCmdTestGuard {
    fn drop(&mut self) {
        TEST_FFMPEG_INSTALL_CMD_OVERRIDE.with(|c| c.set(None));
    }
}

fn ffmpeg_hint_banner_rows() -> u16 {
    if ffmpeg_install_cmd().is_some() { 2 } else { 1 }
}

/// `(image_area, total)` rows for an inline-media preview. Shared by entry-height
/// reservation and block placement so they cannot drift.
pub fn inline_media_reserved_rows(info: &InlineMediaInfo, content_width: u16) -> (u16, u16) {
    use crate::terminal::image::fit_image_to_cells;

    if info.is_video && !ffmpeg_available() {
        let banner_rows = ffmpeg_hint_banner_rows();
        return (banner_rows, banner_rows + 1);
    }
    let max_cols = content_width.saturating_sub(2);
    let max_rows = (content_width / 2).clamp(4, 20);
    let (_cols, rows) = fit_image_to_cells(info.width, info.height, max_cols, max_rows);
    (rows, rows + 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_install_candidates_are_runnable_prompt_hints() {
        for (manager, cmd) in ffmpeg_install_candidates() {
            assert!(!manager.is_empty(), "empty manager name");
            assert!(cmd.starts_with("! "), "not prompt-runnable: {cmd:?}");
            assert!(cmd.contains("ffmpeg"), "missing package: {cmd:?}");
        }
    }

    #[test]
    fn ffmpeg_install_cmd_override_none_omits_command() {
        let _no_pm = set_ffmpeg_install_cmd_for_test(None);
        assert_eq!(ffmpeg_install_cmd(), None);
    }

    #[test]
    fn ffmpeg_install_cmd_override_some_returns_command() {
        let _pm = set_ffmpeg_install_cmd_for_test(Some("! brew install ffmpeg"));
        assert_eq!(ffmpeg_install_cmd(), Some("! brew install ffmpeg"));
    }

    fn media(is_video: bool) -> InlineMediaInfo {
        InlineMediaInfo {
            path: std::path::PathBuf::from("/tmp/x"),
            width: 640,
            height: 480,
            is_video,
            alt_text: String::new(),
        }
    }

    #[test]
    fn reserved_rows_image_is_full_poster_plus_button() {
        let _no_ffmpeg = set_ffmpeg_available_for_test(false);
        let (image_rows, total_rows) = inline_media_reserved_rows(&media(false), 80);
        assert!(
            (4..=20).contains(&image_rows),
            "poster budget: {image_rows}"
        );
        assert_eq!(total_rows, image_rows + 3);
    }

    #[test]
    fn reserved_rows_video_shrinks_to_two_line_banner_with_install_cmd() {
        let _no_ffmpeg = set_ffmpeg_available_for_test(false);
        let _cmd = set_ffmpeg_install_cmd_for_test(Some("! brew install ffmpeg"));
        assert_eq!(inline_media_reserved_rows(&media(true), 80), (2, 3));
    }

    #[test]
    fn reserved_rows_video_shrinks_to_one_line_banner_without_install_cmd() {
        let _no_ffmpeg = set_ffmpeg_available_for_test(false);
        let _no_pm = set_ffmpeg_install_cmd_for_test(None);
        assert_eq!(inline_media_reserved_rows(&media(true), 80), (1, 2));
    }

    #[test]
    fn reserved_rows_video_is_full_poster_with_ffmpeg() {
        let _ffmpeg = set_ffmpeg_available_for_test(true);
        let (image_rows, total_rows) = inline_media_reserved_rows(&media(true), 80);
        assert!(
            (4..=20).contains(&image_rows),
            "poster budget: {image_rows}"
        );
        assert_eq!(total_rows, image_rows + 3);
    }
}
