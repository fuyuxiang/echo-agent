use std::path::Path;

fn profile_directory(out_dir: &Path) -> &Path {
    // Cargo uses build/<crate>-<hash>/out and build/<crate>/<hash>/out.
    // A fixed ancestor count silently selects build/ with the newer layout.
    out_dir
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .expect("ONNX Runtime OUT_DIR must be inside a Cargo build directory")
}

pub fn copy_libraries(lib_dir: &Path, out_dir: &Path) {
    let profile = profile_directory(out_dir);
    for destination in [profile.to_path_buf(), profile.join("examples"), profile.join("deps")] {
        std::fs::create_dir_all(&destination).expect("create ONNX Runtime library destination");
        for entry in std::fs::read_dir(lib_dir).expect("read ONNX Runtime libraries") {
            let entry = entry.expect("read ONNX Runtime library entry");
            let source = entry.path();
            if entry.file_type().expect("read ONNX Runtime library type").is_dir()
                || ![".dll", ".so", ".dylib"].iter().any(|suffix| source.to_string_lossy().contains(suffix))
            {
                continue;
            }
            let output = destination.join(entry.file_name());
            if output.is_symlink() {
                std::fs::remove_file(&output).expect("replace ONNX Runtime library link");
            }
            if !output.exists() {
                #[cfg(windows)]
                if std::os::windows::fs::symlink_file(&source, &output).is_err() {
                    // Test harnesses in deps/ need the DLL too, even when the
                    // runner cannot create symlinks. Do not stop after profile/.
                    std::fs::copy(&source, &output).expect("copy ONNX Runtime library");
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&source, &output).expect("link ONNX Runtime library");
            }
            println!("cargo:rerun-if-changed={}", output.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_old_and_new_cargo_layouts_without_a_fixed_depth() {
        for output in ["target/debug/build/ort-sys-123/out", "target/debug/build/ort-sys/123/out"] {
            assert_eq!(profile_directory(Path::new(output)), Path::new("target/debug"));
        }
        assert_eq!(profile_directory(Path::new("target/x86_64-pc-windows-msvc/release/build/ort-sys/123/out")),
            Path::new("target/x86_64-pc-windows-msvc/release"));
    }

    #[test]
    fn stages_dlls_for_applications_examples_and_test_harnesses_on_a_clean_build() {
        let root = std::env::temp_dir().join(format!("echo-ort-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let library = root.join("libraries");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("DirectML.dll"), b"runtime fixture").unwrap();
        std::fs::write(library.join("not-a-library.txt"), b"ignore").unwrap();
        let profile = root.join("target/release");
        let output = profile.join("build/ort-sys/123/out");
        copy_libraries(&library, &output);
        copy_libraries(&library, &output);
        for directory in [&profile, &profile.join("examples"), &profile.join("deps")] {
            assert_eq!(std::fs::read(directory.join("DirectML.dll")).unwrap(), b"runtime fixture");
            assert!(!directory.join("not-a-library.txt").exists());
        }
        assert!(!profile.join("build/DirectML.dll").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
