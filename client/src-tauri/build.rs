use std::process::Command;

fn check_release_models(manifest_dir: &std::path::Path) {
    let source =
        "../vendor/echo-agent-build/crates/codegen/echo-agent-memory/src/model-assets.json";
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-changed=resources/retrieval-models");
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    // Also protect direct `cargo build --release` / builds using --no-default-features:
    // a release must never silently ship without the fixed, verified weights.
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest_dir.join(source))?)?;
        let root = manifest_dir.join("resources/retrieval-models");
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("verified.json"))?)?;
        if marker != expected {
            return Err("model manifest mismatch".into());
        }
        for asset in expected["assets"]
            .as_array()
            .ok_or("missing model assets")?
        {
            let path = root.join(asset["path"].as_str().ok_or("missing model path")?);
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || Some(metadata.len()) != asset["size"].as_u64()
            {
                return Err(format!("incomplete model: {}", path.display()).into());
            }
            let mut file = std::fs::File::open(&path)?;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 128 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            if format!("{:x}", hash.finalize())
                != asset["sha256"].as_str().ok_or("missing checksum")?
            {
                return Err(format!("model checksum mismatch: {}", path.display()).into());
            }
        }
        Ok(())
    })();
    result.expect("Bundled retrieval models are missing or invalid. Run `pnpm retrieval:prepare` (offline: ECHO_RETRIEVAL_MODELS_SOURCE=/path/to/models) before building.");
}

fn git_output(manifest_dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Render a Unix timestamp as UTC ISO-8601, matching the format the About
/// dialog previously received from `git --format=%cI`. Implemented locally to
/// keep the build script free of a date dependency (Howard Hinnant's
/// `civil_from_days` algorithm).
fn format_utc_iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

fn main() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must set CARGO_MANIFEST_DIR"),
    );
    check_release_models(&manifest_dir);
    let commit = std::env::var("ECHOAGENT_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| git_output(&manifest_dir, &["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    // Wall-clock time of this build. Previously this carried the HEAD commit
    // date (`%cI`), which the About dialog then labelled "build time" — two
    // different facts. The commit date is still exported separately.
    let build_time = std::env::var("ECHOAGENT_BUILD_TIME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| format_utc_iso8601(elapsed.as_secs()))
                .unwrap_or_else(|_| "unknown".into())
        });
    let commit_time = std::env::var("ECHOAGENT_BUILD_COMMIT_TIME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| git_output(&manifest_dir, &["show", "-s", "--format=%cI", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=ECHOAGENT_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=ECHOAGENT_BUILD_TIME={build_time}");
    println!("cargo:rustc-env=ECHOAGENT_BUILD_COMMIT_TIME={commit_time}");
    println!("cargo:rerun-if-env-changed=ECHOAGENT_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=ECHOAGENT_BUILD_TIME");
    println!("cargo:rerun-if-env-changed=ECHOAGENT_BUILD_COMMIT_TIME");
    // An ordinary commit rewrites the branch ref, not `.git/HEAD`; watching HEAD
    // alone left the recorded commit one behind. Watch the ref files, the packed
    // fallback and the index so a commit, checkout or staged change re-runs this
    // script.
    for path in [
        "../.git/HEAD",
        "../.git/refs/heads",
        "../.git/packed-refs",
        "../.git/index",
    ] {
        if manifest_dir.join(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let windows_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    // Keep Tauri's version and icon resources, but provide the application
    // manifest separately below. Tauri otherwise links its manifest only into
    // application binaries, leaving library test harnesses unable to resolve
    // TaskDialogIndirect from Common Controls v6 on Windows.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("failed to run Tauri build script");
    // Link one manifest-only resource into every artifact. The application gets
    // VERSION/icon from Tauri and MANIFEST from this resource; test harnesses
    // get only the manifest. Keeping the resource types disjoint prevents the
    // CVT1100 duplicate-resource failure seen when both contained VERSION 1.
    if windows_target {
        let resource = manifest_dir.join("windows-app-manifest.rc");
        embed_resource::compile_for_everything(resource, embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed Windows resources into test executables");
    }
}
