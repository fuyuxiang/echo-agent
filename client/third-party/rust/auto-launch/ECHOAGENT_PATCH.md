# EchoAgent startup encoding patch

Source: crates.io `auto-launch` 0.5.0, package checksum `1f012b8cc0c850f34117ec8252a44418f2e34a2cf501de89e29b241ae5f79471`, upstream <https://github.com/zzzgydi/auto-launch>. The MIT license and original public API/dependency versions are retained.

Windows login command lines now quote the executable and each argument using the Windows argv backslash/quote rules. macOS LaunchAgent XML now escapes label and argument text. This preserves Chinese/spaces and XML metacharacters in the custom data home and prevents login from reopening an unintended default profile.

The added `serialization` library tests are pure and never write registry/login items. Run them through the desktop workspace with `cargo test --locked --manifest-path src-tauri/Cargo.toml -p auto-launch --lib`. Upstream integration tests are preserved under `tests/`; they require a separately built test executable and modify real OS startup settings, so they were not run during this review. No Windows or actual OS-login acceptance is claimed from serialization tests.

Linux desktop-entry and macOS AppleScript paths are unchanged; EchoAgent uses LaunchAgent on macOS, and Linux is not a formal packaged desktop release target.
