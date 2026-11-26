//! Optional `mmdc` (mermaid-cli) engine, detected at runtime.
//!
//! High-fidelity but heavy (Node + headless Chromium), so it is never selected
//! automatically — a caller opts in via [`MmdcEngine::detect`] / [`MmdcEngine::new`].
//! `mmdc` produces the SVG; we rasterize it through [`crate::rasterize`] so the
//! same security posture (no file resolvers, bundled font) and sizing apply.
//!
//! Security: the subprocess is spawned with [`echo_agent_tty_utils::detach_std_command`]
//! (TTY/session detach) + [`echo_agent_tty_utils::pager_env`] + null stdio, source is
//! passed via a private temp file, and the shared [`crate::run_with_timeout`]
//! enforces a wall-clock budget and reaps the process group (including Chromium
//! grandchildren) on breach.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::subprocess::{SubprocessError, run_with_timeout};
use crate::{MermaidEngine, MermaidError, MermaidTheme, RenderParams, RenderedDiagram};

/// Default wall-clock budget for an `mmdc` invocation.
pub const DEFAULT_MMDC_TIMEOUT: Duration = Duration::from_millis(1500);

/// Locate the `mmdc` binary on `PATH`, if installed.
pub fn detect_mmdc() -> Option<PathBuf> {
    which::which("mmdc").ok()
}

/// An engine that shells out to `mmdc` (mermaid-cli).
///
/// Off by default: construct it explicitly (it requires Node + headless
/// Chromium). Use [`MmdcEngine::detect`] to build one only if `mmdc` is present.
pub struct MmdcEngine {
    bin: PathBuf,
    timeout: Duration,
}

impl MmdcEngine {
    /// Build an engine that runs the `mmdc` binary at `bin`.
    pub fn new(bin: PathBuf) -> Self {
        Self {
            bin,
            timeout: DEFAULT_MMDC_TIMEOUT,
        }
    }

    /// Build an engine if (and only if) `mmdc` is found on `PATH`.
    pub fn detect() -> Option<Self> {
        detect_mmdc().map(Self::new)
    }

    /// Override the wall-clock timeout (default [`DEFAULT_MMDC_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The resolved `mmdc` binary path.
    pub fn binary(&self) -> &Path {
        &self.bin
    }
}

impl MermaidEngine for MmdcEngine {
    fn render(&self, source: &str, params: &RenderParams) -> Result<RenderedDiagram, MermaidError> {
        // Environment/IO failures below are `Rasterize` (a render-pipeline
        // failure), not `Unsupported` (which connotes "this input/engine isn't
        // supported"); spawn failure stays `Unsupported` (engine unavailable).
        let dir = tempfile::Builder::new()
            .prefix("echo-agent-mermaid-")
            .tempdir()
            .map_err(|e| MermaidError::Rasterize(format!("could not create temp dir: {e}")))?;
        let input = dir.path().join("diagram.mmd");
        let output = dir.path().join("diagram.svg");

        // Create atomically with 0600 (no umask/chmod TOCTOU window). The parent
        // tempdir is already 0700.
        write_private(&input, source)
            .map_err(|e| MermaidError::Rasterize(format!("could not write source: {e}")))?;

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .arg("--outputFormat")
            .arg("svg")
            .arg("--theme")
            .arg(theme_arg(params.theme))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .envs(echo_agent_tty_utils::pager_env());
        // setsid/console detach via the sanctioned helper (never a raw pre_exec).
        echo_agent_tty_utils::detach_std_command(&mut cmd);

        // Source goes via the temp file, so no stdin payload.
        run_with_timeout(cmd, None, self.timeout).map_err(map_subprocess_error)?;

        let svg = std::fs::read_to_string(&output).map_err(|e| {
            MermaidError::Layout(format!("mmdc produced no readable SVG output: {e}"))
        })?;
        crate::rasterize(&svg, params)
    }
}

fn theme_arg(theme: MermaidTheme) -> &'static str {
    match theme {
        MermaidTheme::Light => "default",
        MermaidTheme::Dark => "dark",
    }
}

/// Map a subprocess failure onto the engine error taxonomy: a spawn failure
/// means `mmdc` is unavailable ([`MermaidError::Unsupported`]); a non-zero exit
/// is a render/layout failure; a wait failure is a pipeline ([`Rasterize`]) error.
///
/// [`Rasterize`]: MermaidError::Rasterize
fn map_subprocess_error(e: SubprocessError) -> MermaidError {
    match e {
        SubprocessError::Spawn(e) => {
            MermaidError::Unsupported(format!("could not spawn mmdc: {e}"))
        }
        SubprocessError::Timeout => MermaidError::Timeout,
        SubprocessError::NonZeroExit(status) => {
            MermaidError::Layout(format!("mmdc exited with {status}"))
        }
        SubprocessError::Wait(e) => MermaidError::Rasterize(format!("mmdc wait failed: {e}")),
    }
}

/// Write `contents` to `path`, creating it atomically with owner-only (0600)
/// permissions on unix so there is no umask/chmod TOCTOU window.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Write an executable `#!/bin/sh` fake `mmdc` and return (dir-guard, path).
    /// render() invokes `mmdc --input $2 --output $4 --outputFormat svg --theme $8`,
    /// so the script reads `$4` as the output path.
    #[cfg(unix)]
    fn fake_mmdc(body: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mmdc");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (dir, path)
    }

    #[test]
    fn theme_arg_maps_light_and_dark() {
        assert_eq!(theme_arg(MermaidTheme::Light), "default");
        assert_eq!(theme_arg(MermaidTheme::Dark), "dark");
    }

    #[test]
    fn binary_accessor_round_trips() {
        let p = PathBuf::from("/some/path/to/mmdc");
        assert_eq!(MmdcEngine::new(p.clone()).binary(), p.as_path());
    }

    #[test]
    fn map_subprocess_error_preserves_taxonomy() {
        assert!(matches!(
            map_subprocess_error(SubprocessError::Spawn(std::io::Error::other("x"))),
            MermaidError::Unsupported(_)
        ));
        assert!(matches!(
            map_subprocess_error(SubprocessError::Timeout),
            MermaidError::Timeout
        ));
        assert!(matches!(
            map_subprocess_error(SubprocessError::Wait(std::io::Error::other("x"))),
            MermaidError::Rasterize(_)
        ));
    }

    #[test]
    fn missing_binary_is_unsupported() {
        let engine = MmdcEngine::new(PathBuf::from("definitely-not-a-real-binary-9f8a7b6c5d4e"));
        let err = engine
            .render("flowchart LR; A-->B", &RenderParams::default())
            .expect_err("spawning a missing binary must fail");
        assert!(matches!(err, MermaidError::Unsupported(_)));
    }

    #[cfg(unix)]
    #[test]
    fn fake_mmdc_success_produces_decodable_png() {
        let (_dir, bin) = fake_mmdc(
            r##"printf '%s' '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 40 20"><rect width="40" height="20" fill="#445566"/></svg>' > "$4""##,
        );
        let out = MmdcEngine::new(bin)
            .render("flowchart LR; A-->B", &RenderParams::default())
            .expect("fake mmdc output should rasterize");
        assert!(out.width_px > 0 && out.height_px > 0);
        assert!(image::load_from_memory(&out.png).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn fake_mmdc_zero_exit_without_output_is_layout_error() {
        // Exits 0 but writes nothing → the "no readable SVG output" Layout error.
        let (_dir, bin) = fake_mmdc("exit 0");
        let err = MmdcEngine::new(bin)
            .render("flowchart LR; A-->B", &RenderParams::default())
            .expect_err("missing output must error");
        assert!(matches!(err, MermaidError::Layout(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn fake_mmdc_nonzero_exit_maps_to_layout() {
        // A non-zero exit from mmdc surfaces as a Layout error, distinct from a
        // timeout or a missing binary.
        let (_dir, bin) = fake_mmdc("exit 3");
        let err = MmdcEngine::new(bin)
            .render("flowchart LR; A-->B", &RenderParams::default())
            .expect_err("non-zero exit must error");
        assert!(matches!(err, MermaidError::Layout(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn with_timeout_is_honored() {
        // A fake that sleeps far longer than the configured timeout must time out
        // (and be reaped) quickly — proving with_timeout feeds run_with_timeout.
        let (_dir, bin) = fake_mmdc("sleep 30");
        let start = Instant::now();
        let err = MmdcEngine::new(bin)
            .with_timeout(Duration::from_millis(100))
            .render("flowchart LR; A-->B", &RenderParams::default())
            .expect_err("should time out");
        assert!(matches!(err, MermaidError::Timeout));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
