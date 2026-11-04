# EchoAgent integration patch

Source: crates.io `tauri-plugin-updater` 2.9.0, package checksum `27cbc31740f4d507712550694749572ec0e43bdd66992db7599b89fbfd6b167b`, upstream <https://github.com/tauri-apps/plugins-workspace>. Upstream MIT and Apache-2.0 license files are preserved.

`install_without_exit` lets the host save recordings and flush Runtime before installing, then choose its controlled exit. The Windows `ShellExecuteW` result is checked so a rejected/failed launch returns an error and preserves the application; cleanup is never run before a failed launch. Extracted installer files are retained for the external process, matching upstream's immediate-exit lifetime. The existing `install` API still exits on successful Windows installation.

NSIS handoff appends an unquoted `/D=<directory>` as the final argument, after restart/application arguments and configured installer flags. The host supplies a validated durable directory with `UpdaterBuilder::executable_path`; the updater no longer relies on a possibly stale registry path. Spaces and Unicode are preserved, while invalid encoding, quotes and control characters are rejected. MSI arguments are unchanged. The matching EchoAgent NSIS template validates the destination independently and repairs invalid legacy registration and shortcuts before maintenance can invoke an old uninstaller.

Update checks retain HTTP error statuses when all endpoints fail, allowing the host to explain HTTP 410 instead of losing it as a generic missing release. Fallback endpoints are still tried. No server-provided migration URL is automatically adopted.

Signature verification, pinned version, private CA client, archive extraction and other platform installation logic remain upstream 2.9.0. The host calls installation only after verified download. Directory arguments and numerical Windows launch results have portable unit tests. The host has a loopback HTTP 410 integration test. `scripts/validate-windows-installer.ps1` performs real installation, update, legacy-path repair, junction rejection, shortcut and post-cleanup desktop checks exclusively on a disposable Windows CI runner.
