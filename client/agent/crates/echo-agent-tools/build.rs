//! Build script for bundling ripgrep for the echo-agent-tools crate.
//!
//! - If `ECHO_AGENT_TOOLS_BUNDLE_RG_PATH` is set, always bundle it
//! - Otherwise, only bundle in release builds
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Duration;

const RG_VER: &str = "15.2.0";
const BFS_VER: &str = "4.1";
const UGREP_VER: &str = "7.7.0";
const FD_VER: &str = "10.4.2";
// fd stopped publishing x86_64-apple-darwin assets after 10.3.0.
const FD_VER_MACOS_X64: &str = "10.3.0";

/// Pinned SHA-256 of every ripgrep release archive that can be embedded.
/// Keep this table in lock-step with [`RG_VER`]. A release build must fail
/// closed when the selected asset has no trusted digest.
const RG_ARCHIVE_SHA256: &[(&str, &str)] = &[
    (
        "aarch64-apple-darwin",
        "3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4",
    ),
    (
        "x86_64-apple-darwin",
        "af7825fcc69a2afc7a7aea55fc9af90e26421d8f20fe59df32e233c0b8a231c1",
    ),
    (
        "x86_64-unknown-linux-musl",
        "33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "a740b91c82eaf9914cfedd353572f2791cbe0162c84101ee0951058f4dcbc90d",
    ),
    (
        "x86_64-pc-windows-msvc",
        "71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5",
    ),
    (
        "aarch64-pc-windows-msvc",
        "e4abca10c3a64ebea742667dd7009449d49403db5460dd6873e389fa2945360f",
    ),
];

/// Pinned SHA-256 of each `(version, triple)` fd release tarball we embed.
const FD_TARBALL_SHA256: &[(&str, &str, &str)] = &[
    (
        "10.4.2",
        "x86_64-unknown-linux-musl",
        "e3257d48e29a6be965187dbd24ce9af564e0fe67b3e73c9bdcd180f4ec11bdde",
    ),
    (
        "10.4.2",
        "aarch64-unknown-linux-musl",
        "f32d3657473fba74e2600babc8db0b93420d51169223b7e8143b2ed55d8fd9e8",
    ),
    (
        "10.4.2",
        "aarch64-apple-darwin",
        "623dc0afc81b92e4d4606b380d7bc91916ba7b97814263e554d50923a39e480a",
    ),
    (
        "10.3.0",
        "x86_64-apple-darwin",
        "50d30f13fe3d5914b14c4fff5abcbd4d0cdab4b855970a6956f4f006c17117a3",
    ),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    bundle_rg()?;
    // fd is an optional vendored file-search binary backing a feature-gated
    // toolset; skip the download/embed entirely when that feature is off
    // (shipped TUI binaries).
    if env::var_os("CARGO_FEATURE_PI").is_some() {
        bundle_fd()?;
    }
    // bfs/ugrep back the bash-harness find/grep shadows (embedded_search_tools).
    bundle_search_tool("bfs", "BFS", BFS_VER)?;
    bundle_search_tool("ugrep", "UGREP", UGREP_VER)?;
    Ok(())
}

fn download_release_asset(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?;
    for attempt in 1.. {
        match client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::bytes)
        {
            Ok(bytes) => return Ok(bytes.to_vec()),
            Err(error)
                if attempt < 3
                    && error.status().is_none_or(|status| {
                        status.is_server_error() || matches!(status.as_u16(), 408 | 429)
                    }) =>
            {
                println!("cargo:warning=Release asset download attempt {attempt} failed: {error}");
                std::thread::sleep(Duration::from_secs(2 * attempt));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

/// Download + embed fd as an optional vendored file-search binary, mirroring
/// the ripgrep bundling
/// (release-only or `ECHO_AGENT_TOOLS_BUNDLE_FD_PATH` override), plus pinned
/// per-asset SHA-256 verification of the downloaded tarball.
fn bundle_fd() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=ECHO_AGENT_TOOLS_BUNDLE_FD_PATH");
    println!("cargo:rustc-check-cfg=cfg(bundle_fd)");

    let gen_dir = PathBuf::from(env::var("OUT_DIR")?).join("bundle-fd");
    fs::create_dir_all(&gen_dir)?;

    // The consuming vendor extraction is unix-only — never bundle on
    // Windows targets, mirroring the bfs/ugrep skip.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        return Ok(());
    }

    let path_override = env::var("ECHO_AGENT_TOOLS_BUNDLE_FD_PATH").ok();
    let is_release = env::var("PROFILE").as_deref() == Ok("release");
    if path_override.is_none() && !is_release {
        return Ok(());
    }

    // Per-target version: macOS x86_64 pins the last release with that asset.
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (ver, asset_triple) = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => (FD_VER, "aarch64-apple-darwin"),
        ("macos", "x86_64") => (FD_VER_MACOS_X64, "x86_64-apple-darwin"),
        ("linux", "x86_64") => (FD_VER, "x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => (FD_VER, "aarch64-unknown-linux-musl"),
        _ => {
            if path_override.is_none() {
                return Err(format!(
                    "Unsupported target for fd bundling: {target_os}-{target_arch}. Set ECHO_AGENT_TOOLS_BUNDLE_FD_PATH to a local fd binary for offline or unsupported builds.",
                )
                .into());
            }
            (FD_VER, "override")
        }
    };

    println!("cargo:rustc-cfg=bundle_fd");
    println!("cargo:rustc-env=ECHO_AGENT_TOOLS_FD_VER={ver}");

    if let Some(path) = path_override {
        let dest = gen_dir.join(format!("fd-{ver}-override.bin"));
        println!("cargo:rustc-env=ECHO_AGENT_TOOLS_FD_TARGET=override");
        let _ = fs::remove_file(&dest);
        fs::copy(PathBuf::from(path.clone()), &dest).map_err(|e| {
            format!(
                "Failed copying ECHO_AGENT_TOOLS_BUNDLE_FD_PATH: {e} from path {path} to dest {}",
                dest.display()
            )
        })?;
        return Ok(());
    }

    println!("cargo:rustc-env=ECHO_AGENT_TOOLS_FD_TARGET={asset_triple}");
    let dest = gen_dir.join(format!("fd-{ver}-{asset_triple}.bin"));
    let _ = fs::remove_file(&dest);

    let url = format!(
        "https://github.com/sharkdp/fd/releases/download/v{ver}/fd-v{ver}-{asset_triple}.tar.gz"
    );

    let bytes = download_release_asset(&url).map_err(|e| {
        format!(
            "Failed to download fd: {e}\nSet ECHO_AGENT_TOOLS_BUNDLE_FD_PATH to a local fd for offline builds."
        )
    })?;

    // Verify the tarball against the pinned per-asset hash before unpacking.
    let expected_sha = FD_TARBALL_SHA256
        .iter()
        .find(|(v, t, _)| *v == ver && *t == asset_triple)
        .map(|(_, _, sha)| *sha)
        .ok_or_else(|| format!("No pinned SHA-256 for fd {ver} {asset_triple}"))?;
    let actual_sha = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&bytes);
        hex_encode(&hasher.finalize())
    };
    if actual_sha != expected_sha {
        return Err(format!(
            "SHA-256 mismatch for {url}:\n  expected {expected_sha}\n  actual   {actual_sha}"
        )
        .into());
    }

    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut ar = tar::Archive::new(gz);
    let mut found = false;
    for entry in ar.entries()? {
        let mut e = entry?;
        let p = e.path()?;
        if p.file_name().is_some_and(|n| n == "fd") {
            let data: Vec<u8> = {
                let mut v = Vec::new();
                io::copy(&mut e, &mut v)?;
                v
            };
            fs::write(&dest, &data)?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(format!(
            "Could not find 'fd' in fd archive {url}. Set ECHO_AGENT_TOOLS_BUNDLE_FD_PATH for offline builds."
        )
        .into());
    }

    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Bundle a prebuilt **static** search-tool binary (`bfs`/`ugrep`) when
/// `ECHO_AGENT_TOOLS_BUNDLE_<NAME>_PATH` points at one (supplied by the release
/// pipeline). Emits
/// `cfg(bundle_<name>)` so the crate's `include_bytes!` + self-extract engages.
///
/// No auto-download (unlike ripgrep): bfs/ugrep publish no prebuilt static
/// release assets, so the release pipeline supplies the path. Unset → not
/// bundled (the runtime resolver falls back to `~/.echo-agent/vendor` / `$PATH`);
/// never a hard failure, so an un-wired build still succeeds.
fn bundle_search_tool(
    name: &str,
    name_uc: &str,
    ver: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let override_env = format!("ECHO_AGENT_TOOLS_BUNDLE_{name_uc}_PATH");
    println!("cargo:rerun-if-env-changed={override_env}");
    // Always declare the cfg so `#[cfg(bundle_<name>)]` is lint-clean when unset.
    println!("cargo:rustc-check-cfg=cfg(bundle_{name})");

    // The consumer (`embedded_search_tools`) is `#[cfg(unix)]`, so embedding on a
    // Windows target is dead weight — skip (mirrors the ripgrep Windows skip).
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        return Ok(());
    }

    let Some(src) = env::var(&override_env).ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };

    let gen_dir = PathBuf::from(env::var("OUT_DIR")?).join(format!("bundle-{name}"));
    fs::create_dir_all(&gen_dir)?;
    let dest = gen_dir.join(format!("{name}-{ver}-override.bin"));
    let _ = fs::remove_file(&dest);
    fs::copy(&src, &dest)
        .map_err(|e| format!("copy {override_env} from {src} to {}: {e}", dest.display()))?;

    println!("cargo:rustc-cfg=bundle_{name}");
    println!("cargo:rustc-env=ECHO_AGENT_TOOLS_{name_uc}_VER={ver}");
    println!("cargo:rustc-env=ECHO_AGENT_TOOLS_{name_uc}_TARGET=override");
    Ok(())
}

/// Download, verify and embed ripgrep for the current target.
fn bundle_rg() -> Result<(), Box<dyn std::error::Error>> {
    // Only bundle in release builds to avoid slowing down cargo check.
    println!("cargo:rerun-if-env-changed=ECHO_AGENT_TOOLS_BUNDLE_RG_PATH");
    // Declare our custom cfg to the compiler so cfg(bundle_rg) is recognized by lints
    println!("cargo:rustc-check-cfg=cfg(bundle_rg)");

    let gen_dir = PathBuf::from(env::var("OUT_DIR")?).join("bundle-rg");
    fs::create_dir_all(&gen_dir)?;

    // Decide whether to bundle: path override OR release build
    let path_override = env::var("ECHO_AGENT_TOOLS_BUNDLE_RG_PATH").ok();
    let is_release = env::var("PROFILE").as_deref() == Ok("release");
    if path_override.is_none() && !is_release {
        return Ok(());
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Expose cfg so the crate can include the bundled bytes.
    println!("cargo:rustc-cfg=bundle_rg");
    println!("cargo:rustc-env=ECHO_AGENT_TOOLS_RG_VER={}", RG_VER);

    // If a local rg binary is provided, copy it directly (skips target check).
    if let Some(path) = path_override {
        let dest = gen_dir.join(format!("rg-{}-override.bin", RG_VER));
        println!("cargo:rustc-env=ECHO_AGENT_TOOLS_RG_TARGET=override");
        let _ = fs::remove_file(&dest);
        fs::copy(PathBuf::from(path.clone()), &dest).map_err(|e| {
            format!(
                "Failed copying ECHO_AGENT_TOOLS_BUNDLE_RG_PATH: {e} from path {path} to dest {}",
                dest.display()
            )
        })?;
        return Ok(());
    }

    // Determine supported ripgrep asset triple for auto-download.
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (asset_triple, archive_ext, binary_name) = match (target_os.as_str(), target_arch.as_str())
    {
        ("macos", "aarch64") => ("aarch64-apple-darwin", "tar.gz", "rg"),
        ("macos", "x86_64") => ("x86_64-apple-darwin", "tar.gz", "rg"),
        ("linux", "x86_64") => ("x86_64-unknown-linux-musl", "tar.gz", "rg"),
        ("linux", "aarch64") => ("aarch64-unknown-linux-gnu", "tar.gz", "rg"),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", "zip", "rg.exe"),
        ("windows", "aarch64") => ("aarch64-pc-windows-msvc", "zip", "rg.exe"),
        _ => {
            return Err(format!(
                "Unsupported target for ripgrep bundling: {os}-{arch}. Set ECHO_AGENT_TOOLS_BUNDLE_RG_PATH to a local rg binary for offline or unsupported builds.",
                os = target_os,
                arch = target_arch
            ).into());
        }
    };

    println!(
        "cargo:rustc-env=ECHO_AGENT_TOOLS_RG_TARGET={}",
        asset_triple
    );
    let dest = gen_dir.join(format!("rg-{}-{}.bin", RG_VER, asset_triple));
    let _ = fs::remove_file(&dest);

    let url = format!(
        "https://github.com/BurntSushi/ripgrep/releases/download/{v}/ripgrep-{v}-{t}.{archive_ext}",
        v = RG_VER,
        t = asset_triple
    );

    let bytes = download_release_asset(&url).map_err(|e| {
        format!(
            "Failed to download ripgrep: {e}\nSet ECHO_AGENT_TOOLS_BUNDLE_RG_PATH to a local rg for offline builds."
        )
    })?;

    let expected_sha = RG_ARCHIVE_SHA256
        .iter()
        .find(|(target, _)| *target == asset_triple)
        .map(|(_, sha)| *sha)
        .ok_or_else(|| format!("No pinned SHA-256 for ripgrep {RG_VER} {asset_triple}"))?;
    let actual_sha = {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&bytes);
        hex_encode(&hasher.finalize())
    };
    if actual_sha != expected_sha {
        return Err(format!(
            "SHA-256 mismatch for {url}:\n  expected {expected_sha}\n  actual   {actual_sha}"
        )
        .into());
    }

    let embedded = if archive_ext == "zip" {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;
        let mut found = None;
        for idx in 0..archive.len() {
            let mut entry = archive.by_index(idx)?;
            if entry
                .enclosed_name()
                .is_some_and(|path| path.file_name() == Some(std::ffi::OsStr::new(binary_name)))
            {
                let mut data = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut data)?;
                found = Some(data);
                break;
            }
        }
        found
    } else {
        let gz = flate2::read::GzDecoder::new(&bytes[..]);
        let mut archive = tar::Archive::new(gz);
        let mut found = None;
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path()?.file_name() == Some(std::ffi::OsStr::new(binary_name)) {
                let mut data = Vec::new();
                io::copy(&mut entry, &mut data)?;
                found = Some(data);
                break;
            }
        }
        found
    }
    .ok_or_else(|| {
        format!(
            "Could not find '{binary_name}' in ripgrep archive {url}. Set \
             ECHO_AGENT_TOOLS_BUNDLE_RG_PATH for offline builds."
        )
    })?;
    fs::write(&dest, embedded)?;

    Ok(())
}
