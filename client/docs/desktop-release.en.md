# Desktop releases

Release targets are Windows x86_64, macOS Apple Silicon, and macOS Intel. Linux has native source validation but no official installer yet.

## Build and validate

1. Use a native machine or CI runner for each target. Install Node.js 22 or 24, pnpm 10, the required Rust toolchain, protoc, and the platform tools. Run `pnpm install --frozen-lockfile`.
2. Run `pnpm dist:win` on Windows x64 and `pnpm dist:mac` on each macOS architecture. Production packages need platform signing; the macOS build also requires a successful Gatekeeper assessment. `-AllowUnsignedPlatform` and `--allow-unsigned-platform` are for CI and local validation only.
3. The `desktop-validation` workflow runs these release scripts on all three native runners. It installs or mounts the package and checks the WebView, native IPC, and Office export with the bundled Node.js. Confirm all three jobs pass before releasing.
4. On the controlled macOS release machine, run `scripts/prepare-update-artifacts.sh` with the Windows installer and both DMGs. It checks embedded versions, architectures, platform signatures, and all three updater signatures. Do not use `--unsigned` or `--allow-unsigned-platform` for a release.

Run `pnpm test:windows-packaging` on Windows for a quick regression check without compiling an installer. CI checks native warnings, failure exit codes, version output, and checkout paths containing brackets in both Windows PowerShell 5.1 and PowerShell 7. The build script stops on failure and restores the caller's working directory; native warnings with a zero exit code allow it to continue.

Windows NSIS installs per user in `%LOCALAPPDATA%\EchoAgent` by default. Durable custom directories are retained during updates. Temporary folders, browser caches, and junctions into those locations are rejected before maintenance, after directory selection, and before copying files. A legacy temporary registration is repaired to the default directory even if cache cleanup already removed the uninstaller. Product registration, shortcuts and existing startup commands are repaired while tasks, settings, startup arguments and enabled/disabled state are retained. In-app updates explicitly pass a validated directory as the final unquoted `/D=` argument. If the application was already removed, run the repaired full installer to recover it.

`scripts/validate-windows-installer.ps1` runs only on a disposable Windows GitHub Actions runner with no pre-existing product registration. It exercises default installation, custom Chinese paths with spaces and updates, legacy Temp registration with a missing uninstaller, rejection of temporary paths and junctions, shortcuts, retained user data, and real WebView, native IPC, and Office export after removing the test cache. Do not install acceptance builds under Temp in a real user's profile. Review the custom template, based on Tauri CLI 2.12.4, when upgrading the pinned CLI.

The configured update service remains `https://123.56.188.16:8787/desktop-updates/stable/<target>-<arch>.json`. An HTTP 410 is surfaced as an unavailable update route requiring an administrator to check publishing; it is not reported as up-to-date, and response bodies cannot change the configured endpoint.

macOS DMG packaging uses `scripts/macos-dmg.mjs` to record temporary images created by that build. Busy or DiskArbitration timeout failures allow up to three normal detach attempts and one forced attempt, each bounded to 60 seconds. Image identity is checked before every attempt; mismatches, unreadable inventory, and unrelated errors fail immediately. The same handling covers installer verification cleanup. System utilities and other images are untouched. Run `node --test scripts/test-macos-dmg.mjs` for regression checks. To package an existing compiled application again, run `node scripts/macos-dmg.mjs bundle "$PWD/src-tauri/target/release/bundle/macos" "EchoAgent_0.3.11_x64.dmg"`, adjusting the version and using `aarch64` for Apple Silicon; application compilation is not repeated.

Windows packaging requires Visual Studio 2022 C++ x64 tools, MSVC v143 Spectre-mitigated libraries, and the Windows SDK. The packaging script selects VS 2022 through `vswhere`. CI uses `windows-2022` and Python 3.12 to unpack speech assets. Set `PYTHON` to select the Python executable for a local build.


## Update server upgrade

The built-in coding workbench has been retired. Existing conversations are available from history, and workspaces and worktrees remain at their original locations. Open migration details from an old conversation to export plans, drafts, and queued messages. The Windows installer removes known retired program files and preserves other content in those directories. [Legacy workbench data](desktop-recovery.md#旧版工作台数据)

Install the current `deploy/update-server/publish-update.py` and `deploy/update-server/nginx-location.conf` on the update server using `deploy/update-server/install.sh`. The installer copies existing single-target manifests into an initial generation. Nginx then reads the manifests through `stable/current`. Verify all existing update URLs still return their previous manifests before publishing a new version.

`stable/current` is a symlink to a complete manifest generation. The publisher copies and verifies all three update artifacts, writes the next generation, then atomically switches the symlink. A failed copy leaves all visible manifests unchanged. Previous generations remain available for diagnosis and rollback. Do not edit files behind `stable/current` directly.

## Publish

Run `bash scripts/publish-all-updates.sh --artifacts-dir release/v<VERSION> --dry-run`, then repeat without `--dry-run`. The script uploads all three updater artifacts and signatures; the server exposes their manifests together. Fetch each `stable/<target>.json` afterward and verify its version, signature, download URL, and file hash against `artifacts.json` and `SHA256SUMS`.

For a rollback, first verify the previous release files remain downloadable. A release owner can atomically point `stable/current` back to the previous generation and record the old and new generation names.

`pnpm office:build` stages the Office worker, Node.js executable, and Node.js license. The packaged startup check runs with an empty executable search path and verifies Word, Excel, PowerPoint, and PDF export. Windows acceptance also exercises cleanup of recognized retired resources in the registered installation during updates and overwrites, followed by uninstall.
