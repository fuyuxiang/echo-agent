//! REAL host-clipboard plumbing shared by the OS-native paste e2e tests and
//! the `paste_latency` bench: `pbcopy` / `pbpaste` / `osascript` on macOS,
//! PowerShell `Set-Clipboard` / `Get-Clipboard` / WinForms `SetImage` on
//! Windows.
//!
//! Everything here mutates or reads the MACHINE-GLOBAL clipboard, so callers
//! must serialize against each other (e.g. `#[serial_test::serial]`) and
//! should hold a [`HostClipboardTextGuard`] to restore the prior text. CI
//! sessions without a usable clipboard are detected via
//! [`clipboard_roundtrip_works`] so tests can skip instead of fail.
//!
//! Compiles on every platform so cross-platform builds of the consumers stay
//! green (the bench gates at runtime); on unsupported hosts the tool spawns
//! simply fail.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Copy `text` to the host clipboard via `pbcopy`.
#[cfg(not(target_os = "windows"))]
pub fn pbcopy(text: &str) -> Result<()> {
    let mut cmd = Command::new("pbcopy");
    cmd.stdin(Stdio::piped());
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    #[allow(clippy::disallowed_methods)] // short-lived clipboard helper, waited on below
    let mut child = cmd.spawn().context("spawn pbcopy")?;
    child
        .stdin
        .take()
        .context("pbcopy stdin")?
        .write_all(text.as_bytes())
        .context("write pbcopy stdin")?;
    let status = child.wait().context("wait pbcopy")?;
    if !status.success() {
        bail!("pbcopy exited with {status}");
    }
    Ok(())
}

/// Copy `text` to the host clipboard via PowerShell `Set-Clipboard`.
#[cfg(target_os = "windows")]
pub fn pbcopy(text: &str) -> Result<()> {
    // Text travels over stdin, never inside the command line — no quoting.
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Set-Clipboard -Value ([Console]::In.ReadToEnd())",
    ])
    .stdin(Stdio::piped());
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    #[allow(clippy::disallowed_methods)] // short-lived clipboard helper, waited on below
    let mut child = cmd.spawn().context("spawn powershell Set-Clipboard")?;
    child
        .stdin
        .take()
        .context("powershell stdin")?
        .write_all(text.as_bytes())
        .context("write powershell stdin")?;
    let status = child.wait().context("wait powershell Set-Clipboard")?;
    if !status.success() {
        bail!("powershell Set-Clipboard exited with {status}");
    }
    Ok(())
}

/// Current host clipboard TEXT via `pbpaste` (`None` when unavailable).
#[cfg(not(target_os = "windows"))]
pub fn pbpaste() -> Option<String> {
    let mut cmd = Command::new("pbpaste");
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Current host clipboard TEXT via PowerShell `Get-Clipboard` (`None` when
/// unavailable).
#[cfg(target_os = "windows")]
pub fn pbpaste() -> Option<String> {
    let mut cmd = Command::new("powershell");
    // Console::Out.Write avoids the trailing newline PowerShell's pipeline
    // output would append (roundtrip checks compare exact text).
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$c = Get-Clipboard -Raw; if ($null -ne $c) { [Console]::Out.Write($c) }",
    ]);
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Put the PNG at `path` on the host clipboard as a raster (`«class PNGf»`).
#[cfg(not(target_os = "windows"))]
pub fn set_clipboard_png(path: &Path) -> Result<()> {
    let script = format!(
        "set the clipboard to (read (POSIX file \"{}\") as «class PNGf»)",
        path.display()
    );
    let mut cmd = Command::new("osascript");
    cmd.arg("-e").arg(&script);
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    let status = cmd.status().context("spawn osascript")?;
    if !status.success() {
        bail!("osascript set-clipboard-PNG exited with {status}");
    }
    Ok(())
}

/// Put the PNG at `path` on the host clipboard as a raster via a WinForms
/// `Clipboard::SetImage` PowerShell one-liner.
#[cfg(target_os = "windows")]
pub fn set_clipboard_png(path: &Path) -> Result<()> {
    use base64::Engine as _;
    // WinForms Clipboard requires an STA thread; -EncodedCommand (base64 of
    // UTF-16LE) sidesteps every cmd/PowerShell quoting layer, leaving only
    // the PS single-quote escape for the embedded path.
    let ps_path = path.display().to_string().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
         $img = [System.Drawing.Image]::FromFile('{ps_path}'); \
         [System.Windows.Forms.Clipboard]::SetImage($img); \
         $img.Dispose()"
    );
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-STA",
        "-EncodedCommand",
        &encoded,
    ]);
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    let status = cmd.status().context("spawn powershell SetImage")?;
    if !status.success() {
        bail!("powershell SetImage exited with {status}");
    }
    Ok(())
}

/// Write a small solid-color PNG under `dir` and return its path.
pub fn write_fixture_png(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("host_clipboard_fixture.png");
    let buf: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
        image::ImageBuffer::from_pixel(64, 64, image::Rgba([200, 40, 120, 255]));
    buf.save(&path)
        .context("write host clipboard fixture png")?;
    Ok(path)
}

/// Whether the host clipboard actually works in this session: sets a nonce
/// via the text helper and reads it back. False on clipboard-less CI sessions
/// (e.g. a Windows service session with no interactive desktop) so tests can
/// SKIP loudly instead of failing on environment.
pub fn clipboard_roundtrip_works() -> bool {
    let nonce = format!("HOSTCLIPROUNDTRIP{}", std::process::id());
    if pbcopy(&nonce).is_err() {
        return false;
    }
    // trim_end: some clipboard tool chains append a trailing newline.
    pbpaste().is_some_and(|t| t.trim_end() == nonce)
}

/// Best-effort save/restore of the host TEXT clipboard around a test or bench
/// run. Restores on drop (panic/unwind included). A prior IMAGE clipboard
/// cannot be restored — `pbpaste` only reads the text representation.
pub struct HostClipboardTextGuard {
    prior: Option<String>,
}

impl HostClipboardTextGuard {
    pub fn save() -> Self {
        Self { prior: pbpaste() }
    }
}

impl Drop for HostClipboardTextGuard {
    fn drop(&mut self) {
        if let Some(prior) = self.prior.take() {
            // Non-panicking restore: a panicking test must still unwind cleanly.
            let _ = pbcopy(&prior);
        }
    }
}
