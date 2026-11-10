// Keep the bindings from sherpa-onnx-sys 1.13.8 unchanged. Link the verified
// upstream no-TTS distribution prepared by scripts/prepare-meeting-speech.mjs.
fn main() {
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");
    let directory = std::env::var("SHERPA_ONNX_LIB_DIR")
        .expect("Run pnpm meeting:speech:prepare to prepare the native libraries");
    println!("cargo:rerun-if-changed={directory}");
    println!("cargo:rustc-link-search=native={directory}");
    for library in [
        "sherpa-onnx-c-api",
        "sherpa-onnx-core",
        "kaldi-decoder-core",
        "sherpa-onnx-kaldifst-core",
        "sherpa-onnx-fstfar",
        "sherpa-onnx-fst",
        "kaldi-native-fbank-core",
        "kissfft-float",
        "onnxruntime",
        "ssentencepiece_core",
    ] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        Ok("linux") => {
            for library in ["stdc++", "m", "pthread", "dl"] {
                println!("cargo:rustc-link-lib=dylib={library}");
            }
        }
        _ => {}
    }
}
